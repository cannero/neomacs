//! File loading and module system (require/provide/load).

use super::builtins::collections::builtin_make_hash_table;
use super::error::{EvalError, Flow, map_flow, signal};
use super::expr::Expr;
use super::expr::print_expr;
use super::intern::{intern, resolve_sym};
use super::keymap::{is_list_keymap, list_keymap_lookup_one};
use super::value::{HashKey, Value, ValueKind, list_to_vec};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

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
        ValueKind::String => {
            format!("\"{}\"", v.as_str().unwrap_or(""))
        }
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
    args: &[Expr],
) -> Result<Vec<Value>, EvalError> {
    args.iter()
        .map(|expr| eval_runtime_form(eval, expr))
        .collect()
}

fn eval_runtime_form(eval: &mut super::eval::Context, form: &Expr) -> Result<Value, EvalError> {
    let form_value = eval.quote_to_runtime_value(form);
    eval.eval_sub(form_value).map_err(map_flow)
}

fn cached_form_requires_eager_replay(form: Value) -> bool {
    form.is_cons()
        && form
            .cons_car()
            .as_symbol_name()
            .is_some_and(|name| matches!(name, "eval-and-compile" | "eval-when-compile"))
}

fn generated_defalias(eval: &mut super::eval::Context, args: &[Expr]) -> Result<Value, EvalError> {
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
    form: &Expr,
) -> Result<Option<Value>, EvalError> {
    let Expr::List(items) = form else {
        return Ok(None);
    };
    let Some(Expr::Symbol(id)) = items.first() else {
        return Ok(None);
    };
    let tail = &items[1..];
    // Keep this table limited to core primitive replay.  GNU Lisp-owned
    // helpers from loaddefs (e.g. custom/obsolete metadata helpers) should
    // run through the already-loaded GNU Lisp runtime instead.
    match resolve_sym(*id) {
        "progn" => {
            let mut last = Value::NIL;
            for expr in tail {
                last = eval_generated_loaddefs_form(eval, expr)?;
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
    form: &Expr,
) -> Result<Value, EvalError> {
    if let Some(value) = try_eval_generated_loaddefs_form(eval, form)? {
        return Ok(value);
    }
    eval_runtime_form(eval, form)
}

fn has_load_suffix(name: &str) -> bool {
    name.ends_with(".el") || name.ends_with(".elc")
}

fn source_suffixed_path(base: &Path) -> PathBuf {
    let base_str = base.to_string_lossy();
    PathBuf::from(format!("{base_str}.el"))
}

fn compiled_suffixed_path(base: &Path) -> PathBuf {
    let base_str = base.to_string_lossy();
    PathBuf::from(format!("{base_str}.elc"))
}

fn unsupported_compiled_suffixed_paths(base: &Path) -> [PathBuf; 1] {
    let base_str = base.to_string_lossy();
    [PathBuf::from(format!("{base_str}.elc.gz"))]
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
    original_name: &str,
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

/// Search for a file in the load path.
#[tracing::instrument(level = "debug", ret)]
pub fn find_file_in_load_path(name: &str, load_path: &[String]) -> Option<PathBuf> {
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
    load_path: &[String],
    no_suffix: bool,
    must_suffix: bool,
    prefer_newer: bool,
) -> Option<PathBuf> {
    let expanded = expand_tilde(name);
    let path = Path::new(&expanded);
    if path.is_absolute() {
        return find_for_base(path, name, no_suffix, must_suffix, prefer_newer);
    }

    // Emacs searches load-path directory-by-directory; suffix preference
    // is evaluated within each directory.
    for dir in load_path {
        let full = Path::new(dir).join(name);
        if let Some(found) = find_for_base(&full, name, no_suffix, must_suffix, prefer_newer) {
            return Some(found);
        }
    }

    None
}

/// Extract `load-path` from the evaluator's obarray as a Vec<String>.
pub fn get_load_path(obarray: &super::symbol::Obarray) -> Vec<String> {
    let default_directory = obarray
        .symbol_value("default-directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let val = obarray
        .symbol_value("load-path")
        .cloned()
        .unwrap_or(Value::NIL);
    super::value::list_to_vec(&val)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            v if v.is_nil() => Some(default_directory.to_string()),
            _ => v.as_str().map(|s| s.to_string()),
        })
        .collect()
}

pub(crate) enum LoadPlan {
    Return(Value),
    Load { path: PathBuf },
}

pub(crate) fn plan_load_in_state(
    obarray: &super::symbol::Obarray,
    file: Value,
    noerror: Option<Value>,
    nosuffix: Option<Value>,
    must_suffix: Option<Value>,
) -> Result<LoadPlan, Flow> {
    let file = match file.kind() {
        ValueKind::String => file.as_str().unwrap().to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), file],
            ));
        }
    };
    let file = expand_tilde(&file);
    let noerror = noerror.is_some_and(|v| v.is_truthy());
    let nosuffix = nosuffix.is_some_and(|v| v.is_truthy());
    let must_suffix = must_suffix.is_some_and(|v| v.is_truthy());
    let prefer_newer = obarray
        .symbol_value("load-prefer-newer")
        .is_some_and(|v| v.is_truthy());

    let load_path = get_load_path(obarray);
    match find_file_in_load_path_with_flags(&file, &load_path, nosuffix, must_suffix, prefer_newer)
    {
        Some(path) => Ok(LoadPlan::Load { path }),
        None => {
            if noerror {
                Ok(LoadPlan::Return(Value::NIL))
            } else {
                Err(signal(
                    "file-missing",
                    vec![Value::string(format!("Cannot open load file: {}", file))],
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
        LoadPlan::Load { path } => {
            let extra_roots = args.to_vec();
            let noerror = args.get(1).is_some_and(|v| v.is_truthy());
            let nomessage = args.get(2).is_some_and(|v| v.is_truthy());
            shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
                load_file_with_flags(eval, &path, noerror, nomessage).map_err(|e| match e {
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

fn parse_source_forms(source_path: &Path, source: &str) -> Result<Vec<Expr>, EvalError> {
    let (source_for_reader, shebang_only_line) = strip_reader_prefix(source);
    if shebang_only_line {
        return Err(EvalError::Signal {
            symbol: intern("end-of-file"),
            data: vec![],
            raw_data: None,
        });
    }
    super::parser::parse_forms(source_for_reader).map_err(|e| EvalError::Signal {
        symbol: intern("invalid-read-syntax"),
        data: vec![Value::string(format!(
            "Parse error in {}: {:?}",
            source_path.display(),
            e
        ))],
        raw_data: None,
    })
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
    let val = eval.with_gc_scope(|ctx| {
        extra_roots(ctx);
        ctx.root(form_value);
        ctx.root(macroexpand_fn);
        ctx.apply(macroexpand_fn, vec![form_value, Value::NIL]).ok()
    });
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
        if d3.as_millis() > 200 {
            tracing::warn!("eager_expand step3 (full-expand) took {d3:.2?}");
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
                if d4.as_millis() > 200 {
                    tracing::warn!("eager_expand step4 (eval) took {d4:.2?}");
                }
                Ok(value)
            })
        },
    )
}

type ThreadRuntimeRegistries = (
    super::charset::CharsetRegistrySnapshot,
    super::fontset::FontsetRegistrySnapshot,
);

fn snapshot_thread_runtime_registries() -> ThreadRuntimeRegistries {
    (
        super::charset::snapshot_charset_registry(),
        super::fontset::snapshot_fontset_registry(),
    )
}

fn restore_thread_runtime_registries(registries: &ThreadRuntimeRegistries) {
    super::charset::restore_charset_registry(registries.0.clone());
    super::fontset::restore_fontset_registry(registries.1.clone());
}

fn activate_context_thread_runtime(
    ctx: &mut super::eval::Context,
    registries: &ThreadRuntimeRegistries,
) {
    ctx.setup_thread_locals();
    restore_thread_runtime_registries(registries);
}

/// Shared context save/restore for file loading.
///
/// Saves and restores: lexical-binding, lexenv, load-file-name, temp roots.
/// Sets lexical-binding from the file cookie and load-file-name to the path.
/// The `body` closure runs with the new context and its result is returned
/// after context restoration.
fn with_load_context<F>(
    eval: &mut super::eval::Context,
    path: &Path,
    lexical_binding: bool,
    body: F,
) -> Result<Value, EvalError>
where
    F: FnOnce(&mut super::eval::Context) -> Result<Value, EvalError>,
{
    let old_lexical = eval.lexical_binding();
    let old_lexenv = eval.lexenv;
    let old_load_file = eval.obarray().symbol_value("load-file-name").cloned();

    eval.with_gc_scope(|ctx| {
        ctx.root(old_lexenv);
        if let Some(ref v) = old_load_file {
            ctx.root(*v);
        }

        if lexical_binding {
            ctx.set_lexical_binding(true);
            ctx.lexenv = Value::list(vec![Value::T]);
        }

        ctx.set_variable(
            "load-file-name",
            Value::string(path.to_string_lossy().to_string()),
        );

        let result = body(ctx);

        ctx.set_lexical_binding(old_lexical);
        ctx.lexenv = old_lexenv;
        if let Some(old) = old_load_file {
            ctx.set_variable("load-file-name", old);
        } else {
            ctx.set_variable("load-file-name", Value::NIL);
        }

        result
    })
}

/// Shared form-by-form evaluation loop, modelled after GNU Emacs `readevalloop`
/// in lread.c.
///
/// Iterates over `forms`, logging each form and its timing, reporting errors
/// with human-readable detail.
/// The `eval_one` closure controls per-form evaluation semantics (e.g. whether
/// to reify byte-code literals, apply eager macro expansion, or collect expanded
/// forms for caching).
///
/// This function does NOT handle: context save/restore (see `with_load_context`),
/// `record_load_history`, or caching.
fn readevalloop<F>(
    eval: &mut super::eval::Context,
    file_name: &str,
    forms: &[Expr],
    mut eval_one: F,
) -> Result<(), EvalError>
where
    F: FnMut(&mut super::eval::Context, usize, &Expr) -> Result<Value, EvalError>,
{
    for (i, form) in forms.iter().enumerate() {
        tracing::debug!(
            "{} FORM[{i}/{}]: {}",
            file_name,
            forms.len(),
            print_expr(form).chars().take(100).collect::<String>()
        );
        let start = std::time::Instant::now();
        let (h0, m0) = (eval.macro_cache_hits, eval.macro_cache_misses);

        let eval_result = eval_one(eval, i, form);

        let elapsed = start.elapsed();
        let (dh, dm) = (eval.macro_cache_hits - h0, eval.macro_cache_misses - m0);
        if elapsed.as_millis() > 200 || dm > 0 || dh > 0 {
            tracing::debug!(
                "  {file_name} FORM[{i}] ({:.2?}) [cache hit={dh} miss={dm}]: {}",
                elapsed,
                print_expr(form).chars().take(80).collect::<String>()
            );
        }
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
            tracing::error!(
                "  !! {file_name} FORM[{i}] FAILED: {} => {}",
                print_expr(form).chars().take(120).collect::<String>(),
                err_detail
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
    }
    Ok(())
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

    // GNU Emacs only signals `Recursive load` once the same resolved file is
    // already present four times in `Vloads_in_progress`, i.e. on the fifth
    // attempt. Matching that behavior matters because Lisp depends on the
    // error shape rather than on silent skipping.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let load_count = eval
        .loads_in_progress
        .iter()
        .filter(|p| **p == canonical)
        .count();
    if load_count > 3 {
        let in_progress = Value::list(
            eval.loads_in_progress
                .iter()
                .map(|p| Value::string(p.to_string_lossy().to_string()))
                .collect(),
        );
        return Err(EvalError::Signal {
            symbol: intern("error"),
            data: vec![
                Value::string("Recursive load"),
                Value::cons(
                    Value::string(canonical.to_string_lossy().to_string()),
                    in_progress,
                ),
            ],
            raw_data: None,
        });
    }
    eval.loads_in_progress.push(canonical);

    // GNU Emacs lread.c: specbind(Qload_in_progress, Qt)
    // Set load-in-progress to t during file loading, restore afterward.
    let old_load_in_progress = eval
        .obarray()
        .symbol_value("load-in-progress")
        .cloned()
        .unwrap_or(Value::NIL);
    eval.set_variable("load-in-progress", Value::T);

    let result = stacker::maybe_grow(256 * 1024, 32 * 1024 * 1024, || {
        load_file_body(eval, path, noerror, nomessage)
    });

    eval.set_variable("load-in-progress", old_load_in_progress);
    eval.loads_in_progress.pop();
    result
}

fn load_file_body(
    eval: &mut super::eval::Context,
    path: &Path,
    noerror: bool,
    nomessage: bool,
) -> Result<Value, EvalError> {
    let is_elc = path.extension().and_then(|e| e.to_str()) == Some("elc");

    if !is_elc
        && let load_source_file_function =
            eval.visible_variable_value_or_nil("load-source-file-function")
        && !load_source_file_function.is_nil()
    {
        let full_name = Value::string(path.to_string_lossy().to_string());
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
    let content = if is_elc {
        skip_elc_header(&raw_bytes)
    } else {
        decode_emacs_utf8(&raw_bytes)
    };

    // Detect lexical-binding.
    let lexical_binding = if is_elc {
        elc_has_lexical_binding(&raw_bytes)
    } else {
        source_lexical_binding_for_load(
            eval,
            &content,
            Some(Value::string(path.to_string_lossy().to_string())),
        )?
    };

    // --- Shared context setup via with_load_context ---
    with_load_context(eval, path, lexical_binding, |eval| {
        if !is_elc {
            // Clear pointer-identity caches before each source file.
            eval.macro_expansion_cache.clear();
            eval.source_literal_cache.clear();
            return eval_decoded_source_file_in_context(eval, path, &content, lexical_binding);
        }

        // --- Parse forms ---
        let forms = super::parser::parse_forms(&content).map_err(|e| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "Parse error in {}: {}",
                path.display(),
                e
            ))],
            raw_data: None,
        })?;

        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if is_elc {
            tracing::info!(
                "{} parsed {} ELC forms from {} bytes",
                file_name,
                forms.len(),
                content.len()
            );
        }

        // --- .elc path: reify byte-code literals + eval via shared readevalloop ---
        if is_elc {
            readevalloop(eval, &file_name, &forms, |eval, _i, form| {
                let reified = eval
                    .reify_byte_code_literals(form)
                    .map_err(crate::emacs_core::error::map_flow)?;
                eval_runtime_form(eval, &reified)
            })?;
            record_load_history(eval, path);
            return Ok(Value::T);
        }
        unreachable!("non-.elc loads should return earlier");
    })
}

pub(crate) fn eval_decoded_source_file_in_context(
    eval: &mut super::eval::Context,
    path: &Path,
    content: &str,
    lexical_binding: bool,
) -> Result<Value, EvalError> {
    let source_hash = super::file_compile_format::source_sha256(content);
    let runtime_surface_fingerprint = runtime_neobc_surface_fingerprint(eval);
    for neobc_path in neobc_candidate_paths(path) {
        if neobc_path.exists() {
            let expected_surface = runtime_neobc_cache_path(path)
                .as_ref()
                .filter(|runtime_path| *runtime_path == &neobc_path)
                .map(|_| runtime_surface_fingerprint.as_str());
            match super::file_compile_format::read_neobc_with_surface(
                &neobc_path,
                &source_hash,
                expected_surface,
            ) {
                Ok(loaded) => {
                    tracing::info!(
                        "neobc cache hit for {} via {} ({} forms)",
                        path.display(),
                        neobc_path.display(),
                        loaded.forms.len()
                    );
                    let replay_roots: Vec<Value> =
                        loaded.forms.iter().map(|form| form.root_value()).collect();
                    let mut replay_result = Ok(());
                    eval.with_gc_scope(|ctx| {
                        for root in &replay_roots {
                            ctx.root(*root);
                        }
                        for form in &loaded.forms {
                            let step_result: Result<(), EvalError> = match form {
                                super::file_compile_format::LoadedForm::Eval(value) => {
                                    ctx.eval_sub(*value).map(|_| ()).map_err(map_flow)
                                }
                                super::file_compile_format::LoadedForm::EagerEval(value) => {
                                    if let Some(macroexpand_fn) = get_eager_macroexpand_fn(ctx) {
                                        eager_expand_eval(ctx, *value, macroexpand_fn).map(|_| ())
                                    } else {
                                        ctx.eval_sub(*value).map(|_| ()).map_err(map_flow)
                                    }
                                }
                                super::file_compile_format::LoadedForm::Constant(_) => {
                                    // eval-when-compile constant -- already evaluated, skip.
                                    Ok(())
                                }
                            };
                            if let Err(err) = step_result {
                                replay_result = Err(err);
                                break;
                            }
                            ctx.gc_safe_point_exact();
                        }
                    });
                    replay_result?;
                    record_load_history(eval, path);
                    return Ok(Value::T);
                }
                Err(err) => {
                    tracing::debug!(
                        "neobc cache rejected for {} via {}: {}",
                        path.display(),
                        neobc_path.display(),
                        err
                    );
                }
            }
        }
    }

    let generated_loaddefs = is_generated_loaddefs_source(content);
    if generated_loaddefs {
        let forms = parse_source_forms(path, content)?;
        tracing::info!(
            "generated loaddefs replay for {} ({} forms)",
            path.display(),
            forms.len()
        );
        for form in &forms {
            eval_generated_loaddefs_form(eval, form)?;
            eval.gc_safe_point_exact();
        }
        record_load_history(eval, path);
        return Ok(Value::T);
    }

    let macroexpand_fn = get_eager_macroexpand_fn(eval);
    let forms = parse_source_forms(path, content)?;
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let auto_neobc_cache_path =
        runtime_neobc_cache_path(path).unwrap_or_else(|| path.with_extension("neobc"));

    if let Some(mexp_fn) = macroexpand_fn {
        let mut live_runtime_registries = snapshot_thread_runtime_registries();
        let mut lowering_eval =
            super::pdump::clone_active_evaluator(eval).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "failed to clone evaluator for runtime cache lowering: {err}"
                ))],
                raw_data: None,
            })?;
        let mut lowering_runtime_registries = snapshot_thread_runtime_registries();
        activate_context_thread_runtime(eval, &live_runtime_registries);
        let mut compiler_macro_env = Value::NIL;
        let mut neobc_builder = Some(super::file_compile_format::NeobcBuilder::new(
            &source_hash,
            lexical_binding,
        ));
        if let Some(builder) = neobc_builder.as_mut() {
            builder.set_surface_fingerprint(runtime_surface_fingerprint.clone());
        }
        readevalloop(eval, &file_name, &forms, |eval, i, form| {
            activate_context_thread_runtime(eval, &live_runtime_registries);
            let form_value = eval.source_literal_to_runtime_value(form);
            eager_expand_toplevel_forms(
                eval,
                form_value,
                mexp_fn,
                &mut |ctx, original, expanded, requires_eager_replay| {
                    activate_context_thread_runtime(
                        &mut lowering_eval,
                        &lowering_runtime_registries,
                    );
                    let compiled_replay_form = ctx
                        .with_gc_scope_result(|ctx| {
                            ctx.root(original);
                            ctx.root(expanded);
                            lowering_eval.with_gc_scope_result(|lower_ctx| {
                                lower_ctx.root(compiler_macro_env);
                                let (original_local, expanded_local) =
                                    super::file_compile_format::transplant_value_pair(
                                        &original, &expanded,
                                    )
                                    .map_err(|err| {
                                        signal(
                                            "error",
                                            vec![Value::string(format!(
                                                "failed to transplant runtime cache lowering values at {} ({})",
                                                err.path(),
                                                err.detail()
                                            ))],
                                        )
                                    })?;
                                lower_ctx.root(original_local);
                                lower_ctx.root(expanded_local);
                                let compiled = super::file_compile::lower_runtime_cached_toplevel_form_with_env(
                                    lower_ctx,
                                    original_local,
                                    expanded_local,
                                    compiler_macro_env,
                                );
                                if let Some(compiled) = compiled {
                                    lower_ctx.root(compiled);
                                    super::file_compile::maybe_extend_compiler_macro_env_from_lowered(
                                        &mut compiler_macro_env,
                                        compiled,
                                    );
                                    compiler_macro_env = lower_ctx.root(compiler_macro_env);
                                }
                                Ok(compiled)
                            })
                        })
                        .map_err(map_flow)?;
                    lowering_runtime_registries = snapshot_thread_runtime_registries();

                    activate_context_thread_runtime(ctx, &live_runtime_registries);
                    ctx.with_gc_scope(|ctx| {
                        ctx.root(original);
                        ctx.root(expanded);
                        let mut disable_cache = None;
                        if let Some(builder) = neobc_builder.as_mut() {
                            let push_result = if let Some(compiled) = compiled_replay_form {
                                activate_context_thread_runtime(
                                    &mut lowering_eval,
                                    &lowering_runtime_registries,
                                );
                                lowering_eval.with_gc_scope(|lower_ctx| {
                                    lower_ctx.root(compiler_macro_env);
                                    lower_ctx.root(compiled);
                                    builder.push_eval_value_detailed(&compiled)
                                })
                            } else if requires_eager_replay {
                                builder.push_eager_eval_value_detailed(&original)
                            } else {
                                builder.push_eval_value_detailed(&expanded)
                            };
                            if let Err(err) = push_result {
                                disable_cache =
                                    Some((err.path().to_owned(), err.detail().to_owned()));
                            }
                        }
                        if compiled_replay_form.is_some() {
                            lowering_runtime_registries = snapshot_thread_runtime_registries();
                            activate_context_thread_runtime(ctx, &live_runtime_registries);
                        }
                        if let Some((err_path, err_detail)) = disable_cache {
                            tracing::debug!(
                                "neobc cache save skipped for {} at source form {}: unsupported value at {} ({})",
                                path.display(),
                                i,
                                err_path,
                                err_detail
                            );
                            neobc_builder = None;
                        }
                        // Match GNU source loading: cache lowering is a separate
                        // compilation concern. The live load path should still
                        // evaluate the source-expanded form, not the freshly
                        // lowered cache artifact.
                        activate_context_thread_runtime(ctx, &live_runtime_registries);
                        ctx.root(expanded);
                        let t4 = std::time::Instant::now();
                        let value = ctx.eval_value(&expanded).map_err(map_flow)?;
                        live_runtime_registries = snapshot_thread_runtime_registries();
                        let d4 = t4.elapsed();
                        if d4.as_millis() > 200 {
                            tracing::warn!("eager_expand step4 (eval) took {d4:.2?}");
                        }
                        Ok(value)
                    })
                },
            )
        })?;
        if let Some(neobc_builder) = neobc_builder {
            if let Some(parent) = auto_neobc_cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let cached_form_count = neobc_builder.len();
            match neobc_builder.write(&auto_neobc_cache_path) {
                Ok(()) => {
                    tracing::info!(
                        "neobc cache saved for {} to {} ({} forms)",
                        path.display(),
                        auto_neobc_cache_path.display(),
                        cached_form_count
                    );
                }
                Err(err) => {
                    tracing::debug!(
                        "neobc cache save skipped for {} at {}: {}",
                        path.display(),
                        auto_neobc_cache_path.display(),
                        err
                    );
                }
            }
        }
    } else {
        let mut cached_forms = Vec::new();
        readevalloop(eval, &file_name, &forms, |eval, _i, form| {
            cached_forms.push(form.clone());
            eval_runtime_form(eval, form)
        })?;
        if let Some(parent) = auto_neobc_cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match super::file_compile_format::write_neobc_exprs(
            &auto_neobc_cache_path,
            &source_hash,
            lexical_binding,
            &cached_forms,
        ) {
            Ok(()) => {
                tracing::info!(
                    "neobc cache saved for {} to {} ({} forms)",
                    path.display(),
                    auto_neobc_cache_path.display(),
                    cached_forms.len()
                );
            }
            Err(err) => {
                tracing::debug!(
                    "neobc cache save skipped for {} at {}: {}",
                    path.display(),
                    auto_neobc_cache_path.display(),
                    err
                );
            }
        }
    }

    record_load_history(eval, path);

    Ok(Value::T)
}

fn runtime_neobc_surface_fingerprint(eval: &super::eval::Context) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"neomacs-runtime-neobc-surface-v1\0");
    for (name, value) in [
        (
            "features",
            eval.obarray()
                .symbol_value("features")
                .copied()
                .unwrap_or(Value::NIL),
        ),
        (
            "load-history",
            eval.obarray()
                .symbol_value("load-history")
                .copied()
                .unwrap_or(Value::NIL),
        ),
        (
            "macroexp--pending-eager-loads",
            eval.obarray()
                .symbol_value("macroexp--pending-eager-loads")
                .copied()
                .unwrap_or(Value::NIL),
        ),
        (
            "defun-declarations-alist",
            eval.obarray()
                .symbol_value("defun-declarations-alist")
                .copied()
                .unwrap_or(Value::NIL),
        ),
        (
            "load-path",
            eval.obarray()
                .symbol_value("load-path")
                .copied()
                .unwrap_or(Value::NIL),
        ),
    ] {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(super::print::print_value(&value).as_bytes());
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

/// Skip the `;ELC` magic header in a byte-compiled Elisp file.
/// Returns the remaining content as a string.
fn skip_elc_header(raw_bytes: &[u8]) -> String {
    // .elc files start with ";ELC" magic bytes (0x3B 0x45 0x4C 0x43)
    // followed by version bytes (typically 0x1C 0x00 0x00 0x00 for Emacs 28+).
    // Then comment lines starting with ";;".
    //
    // We need to skip all bytes up to the first non-comment line.
    let content = decode_emacs_utf8(raw_bytes);
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

fn record_load_history(eval: &mut super::eval::Context, path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    tracing::info!("record_load_history: {}", path_str);
    eval.with_gc_scope(|eval| {
        // GNU protects the same post-load temporaries with GCPRO/specpdl roots
        // in lread.c. Exact GC needs explicit rooting here as well.
        let path_value = eval.root(Value::string(path_str.clone()));
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
                            .as_str()
                            .is_none_or(|loaded| loaded != path_str)
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
            let abs_path = eval.root(Value::string(path_str.clone()));
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
const BOOTSTRAP_IMAGE_SCHEMA_VERSION: u32 = 16;

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
    pub const fn image_file_name(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap-neomacs.pdump",
            Self::Final => "neomacs.pdump",
        }
    }
}
const NEOBC_CACHE_VERSION: u32 = super::file_compile_format::NEOBC_FORMAT_VERSION;
const LEGACY_NEOBC_CACHE_VERSIONS: &[u32] = &[];
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

pub fn runtime_image_path_for_executable(executable: &Path, role: RuntimeImageRole) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(role.image_file_name())
}

pub fn default_runtime_image_path(role: RuntimeImageRole) -> PathBuf {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .unwrap_or_else(|| PathBuf::from(role.image_file_name()));
    runtime_image_path_for_executable(&executable, role)
}

fn bootstrap_dump_lock_path(dump_path: &Path) -> PathBuf {
    let file_name = dump_path
        .file_name()
        .expect("bootstrap dump path should have file name");
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    dump_path.with_file_name(lock_name)
}

fn runtime_neobc_cache_path_for_version(source_path: &Path, version: u32) -> Option<PathBuf> {
    let runtime_root = runtime_project_root();
    let rel = source_path.strip_prefix(&runtime_root).ok()?;
    Some(
        bootstrap_cache_dir(&runtime_root)
            .join(format!("neobc-v{version}"))
            .join(rel)
            .with_extension("neobc"),
    )
}

fn runtime_neobc_cache_path(source_path: &Path) -> Option<PathBuf> {
    runtime_neobc_cache_path_for_version(source_path, NEOBC_CACHE_VERSION)
}

fn neobc_candidate_paths(source_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(runtime_cache) = runtime_neobc_cache_path(source_path) {
        candidates.push(runtime_cache);
    }
    for version in LEGACY_NEOBC_CACHE_VERSIONS {
        if let Some(legacy_cache) = runtime_neobc_cache_path_for_version(source_path, *version)
            && !candidates
                .iter()
                .any(|candidate| candidate == &legacy_cache)
        {
            candidates.push(legacy_cache);
        }
    }
    let sibling = source_path.with_extension("neobc");
    if !candidates.iter().any(|candidate| candidate == &sibling) {
        candidates.push(sibling);
    }
    candidates
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

fn expr_symbol_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        Expr::List(_) => expr_quoted_symbol_name(expr),
        _ => None,
    }
}

fn expr_quoted_symbol_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        Expr::List(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Expr::Symbol(head), Expr::Symbol(id)) if resolve_sym(*head) == "quote" => {
                Some(resolve_sym(*id).to_owned())
            }
            _ => None,
        },
        _ => None,
    }
}

fn expr_runtime_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Int(v) => Some(Value::fixnum(*v)),
        Expr::Symbol(id) => match resolve_sym(*id) {
            "nil" => Some(Value::NIL),
            "t" => Some(Value::T),
            name => Some(Value::symbol(name)),
        },
        Expr::Keyword(id) => Some(Value::symbol(resolve_sym(*id))),
        Expr::Str(s) => Some(Value::string(s.clone())),
        Expr::Char(c) => Some(Value::char(*c)),
        Expr::List(_) => expr_quoted_symbol_name(expr).map(|name| Value::symbol(&name)),
        _ => None,
    }
}

#[derive(Default)]
struct LoaddefsSurfaceState {
    names: std::collections::BTreeSet<String>,
    autoload_args: Vec<Vec<Value>>,
    property_forms: Vec<Expr>,
    property_keys: std::collections::BTreeSet<(String, String)>,
}

#[derive(Default)]
struct SourceFileSurfaceState {
    function_names: std::collections::BTreeSet<String>,
    variable_names: std::collections::BTreeSet<String>,
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

fn collect_source_surface(expr: &Expr, state: &mut SourceFileSurfaceState) {
    let Expr::List(items) = expr else {
        return;
    };
    let Some(Expr::Symbol(head_id)) = items.first() else {
        return;
    };

    match resolve_sym(*head_id) {
        "progn" | "eval-and-compile" => {
            for item in items.iter().skip(1) {
                collect_source_surface(item, state);
            }
        }
        "defun" | "defmacro" | "defsubst" | "define-inline" => {
            if let Some(name) = items.get(1).and_then(expr_symbol_name) {
                state.function_names.insert(name);
            }
        }
        "defalias" => {
            if let Some(name) = items.get(1).and_then(expr_quoted_symbol_name) {
                state.function_names.insert(name);
            }
        }
        "defvar" | "defconst" | "defcustom" => {
            if let Some(name) = items.get(1).and_then(expr_symbol_name) {
                state.variable_names.insert(name);
            }
        }
        "put" | "function-put" | "define-symbol-prop" => {
            if let Some(name) = items.get(1).and_then(expr_quoted_symbol_name)
                && let Some(prop) = items.get(2).and_then(expr_symbol_name)
            {
                source_surface_insert_property(state, name, prop);
            }
        }
        "def-edebug-elem-spec" => {
            if let Some(name) = items.get(1).and_then(expr_quoted_symbol_name) {
                source_surface_insert_property(state, name, "edebug-form-spec");
            }
        }
        "provide" => {
            if let Some(feature) = items.get(1).and_then(expr_quoted_symbol_name) {
                state.features.insert(feature);
            }
        }
        "pcase-defmacro" => {
            if let Some(name) = items.get(1).and_then(expr_symbol_name) {
                let macroexpander = format!("{name}--pcase-macroexpander");
                state.function_names.insert(macroexpander.clone());
                source_surface_insert_property(state, &macroexpander, "edebug-form-spec");
                source_surface_insert_property(state, name, "pcase-macroexpander");
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
        let forms =
            crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "{error_context}: failed parsing {}: {err}",
                    path.display()
                ))],
                raw_data: None,
            })?;

        for form in &forms {
            collect_source_surface(form, &mut state);
        }
    }

    Ok(state)
}

fn collect_loaddefs_autoload_args(
    expr: &Expr,
    allowed_files: Option<&std::collections::BTreeSet<String>>,
    allowed_names: Option<&std::collections::BTreeSet<String>>,
    state: &mut LoaddefsSurfaceState,
) {
    let Expr::List(items) = expr else {
        return;
    };
    let Some(Expr::Symbol(head_id)) = items.first() else {
        return;
    };
    if resolve_sym(*head_id) != "autoload" {
        return;
    }

    let Some(name) = items.get(1).and_then(expr_quoted_symbol_name) else {
        return;
    };
    let Some(Expr::Str(file)) = items.get(2) else {
        return;
    };
    if let Some(files) = allowed_files
        && !files.contains(file)
    {
        return;
    }
    if let Some(names) = allowed_names
        && !names.contains(&name)
    {
        return;
    }

    state.names.insert(name.clone());
    let mut args = vec![Value::symbol(&name), Value::string(file.clone())];
    for expr in items.iter().skip(3).take(3) {
        let Some(value) = expr_runtime_value(expr) else {
            return;
        };
        args.push(value);
    }
    state.autoload_args.push(args);
}

fn collect_loaddefs_property_forms(
    expr: &Expr,
    names: &std::collections::BTreeSet<String>,
    state: &mut LoaddefsSurfaceState,
) {
    let Expr::List(items) = expr else {
        return;
    };
    let Some(Expr::Symbol(head_id)) = items.first() else {
        return;
    };
    let head = resolve_sym(*head_id);
    if head != "function-put" && head != "put" {
        return;
    }
    let Some(name) = items.get(1).and_then(expr_quoted_symbol_name) else {
        return;
    };
    if names.contains(&name) {
        state.property_forms.push(expr.clone());
        if let Some(prop) = items.get(2).and_then(expr_symbol_name) {
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
        let forms =
            crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "{error_context}: failed parsing {}: {err}",
                    path.display()
                ))],
                raw_data: None,
            })?;

        for form in &forms {
            collect_loaddefs_autoload_args(form, allowed_files, allowed_names, &mut state);
        }
        let property_names = allowed_names
            .cloned()
            .unwrap_or_else(|| state.names.clone());
        for form in &forms {
            collect_loaddefs_property_forms(form, &property_names, &mut state);
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
            let Some(path) = entry.cons_car().as_str().map(ToOwned::to_owned) else {
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
        &[project_root.join("lisp/emacs-lisp/pcase.el")],
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
    let forms =
        crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "ldefs-boot autoload restore: failed parsing {}: {err}",
                ldefs_path.display()
            ))],
            raw_data: None,
        })?;

    let wanted = names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let mut property_forms = Vec::new();

    for form in &forms {
        let Expr::List(items) = form else {
            continue;
        };
        let Some(Expr::Symbol(head_id)) = items.first() else {
            continue;
        };
        if resolve_sym(*head_id) == "autoload"
            && let Some(name) = items.get(1).and_then(expr_quoted_symbol_name)
            && wanted.contains(&name)
        {
            eval_generated_loaddefs_form(eval, form)?;
        }
    }

    for form in &forms {
        let Expr::List(items) = form else {
            continue;
        };
        let Some(Expr::Symbol(head_id)) = items.first() else {
            continue;
        };
        let head = resolve_sym(*head_id);
        if head != "function-put" && head != "put" {
            continue;
        }
        let Some(name) = items.get(1).and_then(expr_quoted_symbol_name) else {
            continue;
        };
        if wanted.contains(&name) {
            property_forms.push(form.clone());
        }
    }

    for form in &property_forms {
        eval_generated_loaddefs_form(eval, form)?;
    }

    Ok(())
}

fn normalize_bootstrap_runtime_surface(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    let compile_only_state = compile_only_cl_loaddefs_state(project_root)?;
    let runtime_loaddefs_state = runtime_loaddefs_restore_state(project_root)?;
    let runtime_source_state = runtime_source_bootstrap_surface_state(project_root)?;
    let runtime_loaded_state =
        runtime_loaded_source_restore_state(eval, project_root, &compile_only_state.names)?;
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
    strip_runtime_icons_surface(eval);

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

    let autoload_entries = eval.autoloads.entries_snapshot();
    for entry in &autoload_entries {
        if strip_names.contains(&entry.name) {
            eval.autoloads.remove(&entry.name);
            let _ = super::builtins::builtin_put(
                eval,
                vec![
                    Value::symbol(&entry.name),
                    Value::symbol("autoload-macro"),
                    Value::NIL,
                ],
            );
        }
    }

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
        eval_runtime_form(eval, form)?;
    }

    Ok(())
}

fn strip_runtime_icons_surface(eval: &mut super::eval::Context) {
    const ICON_RUNTIME_FUNCTIONS: &[&str] = &[
        "define-icon",
        "icons--register",
        "icon-spec-keywords",
        "icon-spec-values",
        "iconp",
        "icon-documentation",
        "icons--spec",
        "icons--copy-spec",
        "icon-complete-spec",
        "icon-string",
        "icon-elements",
        "icons--merge-spec",
        "icons--create",
        "describe-icon",
        "icons--describe-spec",
    ];
    const ICON_RUNTIME_VARIABLES: &[&str] = &["icon-preference", "icon", "icon-button"];
    const ICON_RUNTIME_FACES: &[&str] = &["icon", "icon-button"];

    eval.remove_feature("icons");

    for name in ICON_RUNTIME_FUNCTIONS {
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

    for name in ICON_RUNTIME_VARIABLES {
        eval.obarray_mut().makunbound(name);
    }

    for face in ICON_RUNTIME_FACES {
        super::font::clear_created_lisp_face(face);
    }
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
        frame
            .parameters
            .entry("display-type".to_string())
            .or_insert(Value::symbol("color"));
        frame
            .parameters
            .entry("background-mode".to_string())
            .or_insert(Value::symbol("light"));
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

fn eval_first_form_after_marker(
    eval: &mut super::eval::Context,
    path: &Path,
    source: &str,
    marker: &str,
) -> Result<(), EvalError> {
    let start = source.find(marker).ok_or_else(|| EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "runtime prefix restore: missing GNU subr marker {marker}"
        ))],
        raw_data: None,
    })?;
    let forms = crate::emacs_core::parser::parse_forms(&source[start..]).map_err(|err| {
        EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "runtime prefix restore: failed parsing {} from {marker}: {err:?}",
                path.display()
            ))],
            raw_data: None,
        }
    })?;
    let form = forms.first().ok_or_else(|| EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "runtime prefix restore: no GNU subr form after {marker}"
        ))],
        raw_data: None,
    })?;
    eval_runtime_form(eval, form)?;
    Ok(())
}

fn restore_runtime_prefix_keymaps_from_subr(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    let required_pairs = [
        ("ESC-prefix", "esc-map"),
        ("Control-X-prefix", "ctl-x-map"),
        ("ctl-x-4-prefix", "ctl-x-4-map"),
        ("ctl-x-5-prefix", "ctl-x-5-map"),
    ];

    let needs_restore = required_pairs.iter().any(|(alias, variable)| {
        let alias_ok = eval
            .obarray()
            .symbol_function(alias)
            .is_some_and(is_list_keymap);
        let variable_ok = eval
            .obarray()
            .symbol_value(variable)
            .is_some_and(is_list_keymap);
        !(alias_ok && variable_ok)
    });
    if !needs_restore {
        return Ok(());
    }

    let subr_path = project_root.join("lisp/subr.el");
    let subr_source = fs::read_to_string(&subr_path).map_err(|err| EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "runtime prefix restore: failed reading {}: {err}",
            subr_path.display()
        ))],
        raw_data: None,
    })?;

    for marker in [
        "(defvar esc-map",
        "(fset 'ESC-prefix esc-map)",
        "(defvar ctl-x-4-map",
        "(defalias 'ctl-x-4-prefix ctl-x-4-map)",
        "(defvar ctl-x-5-map",
        "(defalias 'ctl-x-5-prefix ctl-x-5-map)",
        "(defvar ctl-x-map",
        "(fset 'Control-X-prefix ctl-x-map)",
        "(defvar global-map",
        "(use-global-map global-map)",
    ] {
        eval_first_form_after_marker(eval, &subr_path, &subr_source, marker)?;
    }

    Ok(())
}

fn runtime_global_prefix_links_need_repair(eval: &super::eval::Context) -> bool {
    let global = eval
        .obarray()
        .symbol_value("global-map")
        .copied()
        .filter(is_list_keymap);
    let Some(global) = global else {
        return true;
    };

    let esc = list_keymap_lookup_one(&global, &Value::fixnum(27));
    let ctl_x = list_keymap_lookup_one(&global, &Value::fixnum(24));
    esc.as_symbol_name() != Some("ESC-prefix") || ctl_x.as_symbol_name() != Some("Control-X-prefix")
}

fn repair_runtime_global_prefix_links(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    if !runtime_global_prefix_links_need_repair(eval) {
        return Ok(());
    }

    for rel_path in ["lisp/subr.el", "lisp/bindings.el"] {
        let path = project_root.join(rel_path);
        let source = fs::read_to_string(&path).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "runtime global prefix repair: failed reading {}: {err}",
                path.display()
            ))],
            raw_data: None,
        })?;
        let forms =
            crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "runtime global prefix repair: failed parsing {}: {err:?}",
                    path.display()
                ))],
                raw_data: None,
            })?;
        for form in &forms {
            let Expr::List(items) = form else {
                continue;
            };
            let Some(Expr::Symbol(head_id)) = items.first() else {
                continue;
            };
            if resolve_sym(*head_id) != "define-key" {
                continue;
            }
            let Some(Expr::Symbol(map_id)) = items.get(1) else {
                continue;
            };
            if resolve_sym(*map_id) != "global-map" {
                continue;
            }
            eval_runtime_form(eval, form)?;
        }
    }

    Ok(())
}

fn finalize_cached_bootstrap_eval(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    // Register all builtins — pdump doesn't preserve live Rust entry-point
    // pointers on heap subr objects, so the callable surface must be rebuilt.
    super::builtins::init_builtins(eval);
    // Restore the created-lisp-faces set from the face table — pdump
    // doesn't preserve the thread-local CREATED_LISP_FACES HashSet, so
    // faces like 'warning (defined by defface in faces.el) would be
    // unrecognized without this.
    super::font::restore_created_faces_from_table(&eval.face_table.face_list());
    clear_runtime_loader_state(eval);
    ensure_startup_compat_variables(eval, project_root);
    restore_cached_runtime_window_system_surface(eval);
    normalize_bootstrap_runtime_surface(eval, project_root)?;
    restore_runtime_prefix_keymaps_from_subr(eval, project_root)?;
    repair_runtime_global_prefix_links(eval, project_root)?;

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

fn eval_startup_forms(eval: &mut super::eval::Context, forms_src: &str) -> Result<(), EvalError> {
    let forms =
        crate::emacs_core::parser::parse_forms(forms_src).map_err(|e| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!("startup parse error: {e}"))],
            raw_data: None,
        })?;
    for result in eval.eval_forms(&forms) {
        result?;
    }
    Ok(())
}

/// Apply the runtime startup state that GNU Emacs has after the dumped image
/// is loaded and `normal-top-level` begins to run.
///
/// The dumped bootstrap image intentionally stops before normal interactive
/// startup.  Runtime callers that compare against `emacs --batch -Q` still
/// need the early startup buffer initialization that `startup.el` performs for
/// the `*scratch*` buffer.
pub fn apply_runtime_startup_state(eval: &mut super::eval::Context) -> Result<(), EvalError> {
    let project_root = runtime_project_root();
    eval_startup_forms(
        eval,
        r#"
          (if (get-buffer "*scratch*")
              (with-current-buffer "*scratch*"
                (if (eq major-mode 'fundamental-mode)
                    (funcall initial-major-mode))))
          ;; GNU loadup.el gates this on compiled-function-p, but NeoVM
          ;; loads .el source (no .elc), so cconv functions are interpreted.
          ;; We set the filter unconditionally when cconv-fv is fboundp so
          ;; that interpreted closure shapes match GNU Emacs.
          (when (and (null internal-make-interpreted-closure-function)
                     (fboundp 'cconv-fv))
            (setq internal-make-interpreted-closure-function
                  #'cconv-make-interpreted-closure))
        "#,
    )?;

    // GNU's startup path reaches its post-startup surface through compiled
    // early Lisp. NeoVM executes the same files from source, which can
    // transiently reload compile-time helpers such as `gv`. Normalize the
    // runtime-visible autoload/feature surface again after those forms run.
    normalize_bootstrap_runtime_surface(eval, &project_root)?;

    let filter_fn = eval
        .obarray()
        .symbol_value("internal-make-interpreted-closure-function")
        .cloned()
        .and_then(|value| {
            if value.is_symbol_named("cconv-make-interpreted-closure") {
                eval.obarray()
                    .symbol_function("cconv-make-interpreted-closure")
                    .cloned()
            } else {
                None
            }
        });
    eval.set_interpreted_closure_filter_fn(filter_fn);
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

pub fn create_bootstrap_evaluator() -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_with_features(&[])
}

/// Create a pre-loadup context for GNU source bootstrap.
///
/// This keeps ordinary `Context::new()` close to GNU's C-level startup
/// surface while still letting NeoVM load `byte-run.el` from source, where
/// only `eval-and-compile` is needed before its later `defmacro`.
pub fn create_source_bootstrap_context() -> super::eval::Context {
    let mut eval = super::eval::Context::new();
    super::bootstrap_macros::install_bootstrap_macro_function_cells(&mut eval);
    eval
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
    stacker::maybe_grow(256 * 1024, 32 * 1024 * 1024, || {
        maybe_trace_bootstrap_step("create_bootstrap_evaluator_with_features: enter");
        let mut eval = create_source_bootstrap_context();
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
        // Override Elisp function-get with Rust builtin to avoid deep
        // eval depth consumption. The Elisp version from subr.el uses
        // get/fboundp/symbol-function which each increment depth in NeoVM
        // (but not in GNU's C implementations).
        eval.obarray
            .set_symbol_function("function-get", Value::subr(intern("function-get")));
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
                if let Ok(forms) = crate::emacs_core::parser::parse_forms(stub) {
                    let _ = eval.eval_forms(&forms);
                }
            }
        }

        // Load loadup.el — this does everything GNU's loadup.el does:
        // loads all core .el/.elc files, handles platform conditionals,
        // manages eager expansion, etc.
        let loadup_path = lisp_dir.join("loadup.el");
        tracing::info!("Loading loadup.el from {}", loadup_path.display());
        match load_file(&mut eval, &loadup_path) {
            Ok(_) => tracing::info!("loadup.el completed successfully"),
            Err(e) => {
                let rendered = format_eval_error_in_state(&eval, &e);
                tracing::error!("loadup.el failed: {rendered}");
                maybe_trace_bootstrap_step(format!(
                    "create_bootstrap_evaluator_with_features: loadup-failed={rendered}"
                ));
                // If kill-emacs was called (setting shutdown_request) during
                // loadup.el, any subsequent errors (e.g. from post-dump code
                // like `(eval top-level t)`) are expected and can be ignored.
                if eval.shutdown_request.is_some() {
                    tracing::info!(
                        "loadup.el completed (shutdown requested, ignoring post-dump error: {e:?})"
                    );
                } else {
                    // Check for our special exit signal from kill-emacs
                    match &e {
                        EvalError::Signal { symbol, .. }
                            if resolve_sym(*symbol) == "kill-emacs" =>
                        {
                            tracing::info!("loadup.el completed (kill-emacs after dump)");
                        }
                        _ => {
                            return Err(e);
                        }
                    }
                }
            }
        }

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
    use super::pdump;

    let project_root = runtime_project_root();
    let dump_path = dump_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_runtime_image_path(role));
    let mut eval = pdump::load_from_dump(&dump_path).map_err(|err| {
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
        // Early runtime-image load happens before a tagged heap is installed on
        // the current thread. Represent the startup failure with an interned
        // raw symbol payload instead of allocating a Lisp string or list here.
        EvalError::Signal {
            symbol: intern("error"),
            data: vec![payload],
            raw_data: Some(payload),
        }
    })?;

    if !extra_features.is_empty() {
        let bootstrap_features = normalized_bootstrap_features(extra_features);
        for feature in &bootstrap_features {
            let _ = eval.provide_value(Value::symbol(feature), None);
        }
    }

    finalize_cached_bootstrap_eval(&mut eval, &project_root)?;
    Ok(eval)
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
