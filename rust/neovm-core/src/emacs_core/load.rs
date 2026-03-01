//! File loading and module system (require/provide/load).

use super::error::{map_flow, EvalError};
use super::eval::quote_to_value;
use super::expr::print_expr;
use super::expr::Expr;
use super::intern::intern;
use super::value::Value;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Decode Emacs "extended UTF-8" bytes into a Rust String.
///
/// Emacs uses a superset of UTF-8 that allows code points above U+10FFFF
/// (used for internal charset characters, eight-bit raw bytes, etc.).
/// These are encoded as standard 4-byte UTF-8 sequences with first byte
/// F5-F7 (covering U+140000-U+1FFFFF), which standard UTF-8 rejects.
///
/// For `?<extended>` character literals, we replace the extended bytes
/// with `?\x<HEX>` escape syntax that the parser already supports.
/// All other extended byte sequences (outside `?` context) are replaced
/// with U+FFFD, matching lossy UTF-8 behaviour.
fn decode_emacs_utf8(bytes: &[u8]) -> String {
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
        if b >= 0xE0 && b <= 0xEF && i + 2 < bytes.len()
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
        if b >= 0xF0 && b <= 0xF4 && i + 3 < bytes.len()
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
        if b >= 0xF5 && b <= 0xF7 && i + 3 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
        {
            let cp = ((b as u32 & 0x07) << 18)
                | ((bytes[i + 1] as u32 & 0x3F) << 12)
                | ((bytes[i + 2] as u32 & 0x3F) << 6)
                | (bytes[i + 3] as u32 & 0x3F);
            // Check if preceded by `?` — this is a character literal.
            if out.ends_with('?') {
                // Replace the extended char with `\x<HEX>` escape so the
                // parser reads it as an integer code point.
                out.push_str(&format!("\\x{:X}", cp));
            } else {
                // Outside character literal context, use replacement char.
                out.push('\u{FFFD}');
            }
            i += 4;
            continue;
        }
        // Invalid byte — replacement character.
        out.push('\u{FFFD}');
        i += 1;
    }
    out
}

/// Format a Value for human-readable error messages, resolving SymIds and ObjIds.
fn format_value_for_error(v: &Value) -> String {
    match v {
        Value::Symbol(sid) => super::intern::resolve_sym(*sid).to_string(),
        Value::Keyword(sid) => super::intern::resolve_sym(*sid).to_string(),
        Value::Str(id) => {
            super::value::with_heap(|h: &crate::gc::LispHeap| {
                format!("\"{}\"", h.get_string(*id))
            })
        }
        Value::Int(n) => format!("{}", n),
        Value::Char(c) => format!("?{}", c),
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        Value::Cons(id) => {
            super::value::with_heap(|h: &crate::gc::LispHeap| {
                let car = h.cons_car(*id);
                let cdr = h.cons_cdr(*id);
                let car_s = format_value_for_error(&car);
                let cdr_s = format_value_for_error(&cdr);
                if cdr == Value::Nil {
                    format!("({})", car_s)
                } else {
                    format!("({} . {})", car_s, cdr_s)
                }
            })
        }
        other => format!("{:?}", other),
    }
}

fn has_load_suffix(name: &str) -> bool {
    name.ends_with(".el")
}

fn source_suffixed_path(base: &Path) -> PathBuf {
    let base_str = base.to_string_lossy();
    PathBuf::from(format!("{base_str}.el"))
}

fn unsupported_compiled_suffixed_paths(base: &Path) -> [PathBuf; 2] {
    let base_str = base.to_string_lossy();
    [
        PathBuf::from(format!("{base_str}.elc")),
        PathBuf::from(format!("{base_str}.elc.gz")),
    ]
}

fn pick_suffixed(base: &Path, _prefer_newer: bool) -> Option<PathBuf> {
    let el = source_suffixed_path(base);
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

    // Surface unsupported compiled artifacts explicitly instead of reporting
    // generic file-missing when only `.elc` payloads are present.
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
/// - `prefer_newer`: kept for API compatibility; no effect in source-only mode.
/// - default: search each load-path directory in order, preferring suffixed
///   files within each directory before bare names.
pub fn find_file_in_load_path_with_flags(
    name: &str,
    load_path: &[String],
    no_suffix: bool,
    must_suffix: bool,
    prefer_newer: bool,
) -> Option<PathBuf> {
    let path = Path::new(name);
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
        .unwrap_or(Value::Nil);
    super::value::list_to_vec(&val)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            Value::Nil => Some(default_directory.to_string()),
            _ => v.as_str().map(|s| s.to_string()),
        })
        .collect()
}

const ELISP_CACHE_MAGIC: &str = "NEOVM-ELISP-CACHE-V1";
const ELISP_CACHE_SCHEMA: &str = "schema=1";
const ELISP_CACHE_VM_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ELISP_CACHE_EXTENSION: &str = "neoc";
const ELISP_CACHE_TEMP_EXTENSION: &str = "neoc.tmp";

fn cache_key(lexical_binding: bool) -> String {
    let lexical = if lexical_binding { "1" } else { "0" };
    format!("{ELISP_CACHE_SCHEMA};vm={ELISP_CACHE_VM_VERSION};lexical={lexical}")
}

fn source_hash(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn cache_sidecar_path(source_path: &Path) -> PathBuf {
    source_path.with_extension(ELISP_CACHE_EXTENSION)
}

fn cache_temp_path(source_path: &Path) -> PathBuf {
    cache_sidecar_path(source_path).with_extension(ELISP_CACHE_TEMP_EXTENSION)
}

const CACHE_WRITE_PHASE_BEFORE_WRITE: u8 = 1;
const CACHE_WRITE_PHASE_AFTER_WRITE: u8 = 2;

#[cfg(test)]
thread_local! {
    static CACHE_WRITE_FAIL_PHASE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn set_cache_write_fail_phase_for_test(phase: u8) {
    CACHE_WRITE_FAIL_PHASE.with(|p| p.set(phase));
}

#[cfg(test)]
fn clear_cache_write_fail_phase_for_test() {
    CACHE_WRITE_FAIL_PHASE.with(|p| p.set(0));
}

fn maybe_inject_cache_write_failure(_phase: u8) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let should_fail = CACHE_WRITE_FAIL_PHASE.with(|p| p.get() == _phase);
        if should_fail {
            return Err(std::io::Error::other(format!(
                "injected .{ELISP_CACHE_EXTENSION} write failure at phase {_phase}"
            )));
        }
    }

    Ok(())
}

fn best_effort_sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_data();
        }
    }
}

fn maybe_load_cached_forms(
    source_path: &Path,
    source: &str,
    lexical_binding: bool,
) -> Option<Vec<Expr>> {
    let cache_path = cache_sidecar_path(source_path);
    let raw = fs::read_to_string(cache_path).ok()?;
    let mut parts = raw.splitn(5, '\n');
    let magic = parts.next()?;
    let key = parts.next()?;
    let hash = parts.next()?;
    let blank = parts.next()?;
    let payload = parts.next().unwrap_or("");

    if magic != ELISP_CACHE_MAGIC {
        return None;
    }
    if !blank.is_empty() {
        return None;
    }

    let expected_key = format!("key={}", cache_key(lexical_binding));
    if key != expected_key {
        return None;
    }
    let expected_hash = format!("source-hash={:016x}", source_hash(source));
    if hash != expected_hash {
        return None;
    }

    super::parser::parse_forms(payload).ok()
}

fn write_forms_cache(
    source_path: &Path,
    source: &str,
    lexical_binding: bool,
    forms: &[Expr],
) -> std::io::Result<()> {
    let cache_path = cache_sidecar_path(source_path);
    let payload = forms.iter().map(print_expr).collect::<Vec<_>>().join("\n");
    let raw = format!(
        "{ELISP_CACHE_MAGIC}\nkey={}\nsource-hash={:016x}\n\n{}\n",
        cache_key(lexical_binding),
        source_hash(source),
        payload
    );

    let tmp_path = cache_temp_path(source_path);
    let write_result = (|| -> std::io::Result<()> {
        maybe_inject_cache_write_failure(CACHE_WRITE_PHASE_BEFORE_WRITE)?;

        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(raw.as_bytes())?;
        file.sync_data()?;

        maybe_inject_cache_write_failure(CACHE_WRITE_PHASE_AFTER_WRITE)?;

        fs::rename(&tmp_path, &cache_path)?;
        best_effort_sync_parent_dir(&cache_path);
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

fn parse_source_with_cache(
    source_path: &Path,
    source: &str,
    lexical_binding: bool,
) -> Result<Vec<Expr>, EvalError> {
    if let Some(forms) = maybe_load_cached_forms(source_path, source, lexical_binding) {
        return Ok(forms);
    }

    let forms = parse_source_forms(source_path, source)?;
    // Cache persistence failures must not affect `load` semantics.
    if load_cache_writes_enabled() {
        let _ = write_forms_cache(source_path, source, lexical_binding, &forms);
    }
    Ok(forms)
}

fn cache_write_disabled_env_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
}

fn load_cache_writes_enabled() -> bool {
    match std::env::var("NEOVM_DISABLE_LOAD_CACHE_WRITE") {
        Ok(value) => !cache_write_disabled_env_value(&value),
        Err(_) => true,
    }
}

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
    let Some(start) = line.find("-*-") else {
        return false;
    };
    let rest = &line[start + 3..];
    let Some(end_rel) = rest.find("-*-") else {
        return false;
    };
    let cookie = &rest[..end_rel];

    for entry in cookie.split(';') {
        let Some((name, value)) = entry.split_once(':') else {
            continue;
        };
        if name.trim() == "lexical-binding" {
            return value.trim() == "t";
        }
    }
    false
}

fn lexical_binding_enabled_for_source(source: &str) -> bool {
    let mut lines = strip_utf8_bom(source).lines();
    let first_line = lines.next();
    if first_line.is_some_and(lexical_binding_enabled_in_file_local_cookie_line) {
        return true;
    }

    if first_line.is_some_and(|line| line.starts_with("#!")) {
        return lines
            .next()
            .is_some_and(lexical_binding_enabled_in_file_local_cookie_line);
    }

    false
}

fn parse_source_forms(source_path: &Path, source: &str) -> Result<Vec<Expr>, EvalError> {
    let (source_for_reader, shebang_only_line) = strip_reader_prefix(source);
    if shebang_only_line {
        return Err(EvalError::Signal {
            symbol: intern("end-of-file"),
            data: vec![],
        });
    }
    super::parser::parse_forms(source_for_reader).map_err(|e| EvalError::Signal {
        symbol: intern("invalid-read-syntax"),
        data: vec![Value::string(format!(
            "Parse error in {}: {:?}",
            source_path.display(),
            e
        ))],
    })
}

fn is_unsupported_compiled_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.ends_with(".elc") || name.ends_with(".elc.gz")
}

/// Parse and precompile a source `.el` file into a `.neoc` sidecar cache.
///
/// The emitted cache is an internal NeoVM artifact and not a compatibility
/// boundary. Failures to persist cache are reported to callers.
pub fn precompile_source_file(source_path: &Path) -> Result<PathBuf, EvalError> {
    if is_unsupported_compiled_path(source_path) {
        return Err(EvalError::Signal {
            symbol: intern("file-error"),
            data: vec![Value::string(format!(
                "Precompile input must be source (.el), not compiled artifacts (.elc/.elc.gz): {}",
                source_path.display()
            ))],
        });
    }

    let raw_bytes = fs::read(source_path).map_err(|e| EvalError::Signal {
        symbol: intern("file-error"),
        data: vec![Value::string(format!(
            "Cannot read source file for precompile: {}: {}",
            source_path.display(),
            e
        ))],
    })?;
    let content = decode_emacs_utf8(&raw_bytes);

    let lexical_binding = lexical_binding_enabled_for_source(&content);
    let forms = parse_source_forms(source_path, &content)?;

    write_forms_cache(source_path, &content, lexical_binding, &forms).map_err(|e| {
        EvalError::Signal {
            symbol: intern("file-error"),
            data: vec![Value::string(format!(
                "Failed to persist precompile cache {}: {}",
                cache_sidecar_path(source_path).display(),
                e
            ))],
        }
    })?;

    Ok(cache_sidecar_path(source_path))
}

/// Check if eager macro expansion is available.
/// Requires both `internal-macroexpand-for-load` and the pcase backquote
/// macroexpander (`--pcase-macroexpander`) to be defined, since
/// `macroexpand-all` uses pcase backquote patterns internally.
#[tracing::instrument(level = "debug", skip(eval))]
fn get_eager_macroexpand_fn(eval: &super::eval::Evaluator) -> Option<Value> {
    // Respect the Elisp `macroexp--pending-eager-loads` variable.
    // When it starts with `skip`, eager expansion is suppressed (mirrors
    // the check in `internal-macroexpand-for-load` in macroexp.el).
    if let Some(val) = eval.obarray().symbol_value("macroexp--pending-eager-loads") {
        if let Value::Cons(id) = val {
            if eval.heap.cons_car(*id).is_symbol_named("skip") {
                return None;
            }
        }
    }
    // Guard: pcase ` macroexpander must be available
    eval.obarray()
        .symbol_function("`--pcase-macroexpander")?;
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
#[tracing::instrument(level = "debug", skip(eval, form_value, macroexpand_fn))]
fn eager_expand_eval(
    eval: &mut super::eval::Evaluator,
    form_value: Value,
    macroexpand_fn: Value,
) -> Result<Value, EvalError> {
    // Step 1: one-level expand — val = (internal-macroexpand-for-load val nil)
    // Note: real Emacs mutates `val` here; we shadow it.
    let saved = eval.save_temp_roots();
    eval.push_temp_root(form_value);
    eval.push_temp_root(macroexpand_fn);
    let val = match eval.apply(macroexpand_fn, vec![form_value, Value::Nil]) {
        Ok(v) => v,
        Err(_) => {
            // Eager expansion failed (cycle detection, missing macro, etc.).
            // Fall back to evaluating the original form without expansion.
            // This matches .elc behavior where forms are already compiled.
            eval.restore_temp_roots(saved);
            tracing::debug!("eager_expand step1 failed, falling back to plain eval");
            return eval.eval_value(&form_value).map_err(map_flow);
        }
    };
    eval.restore_temp_roots(saved);

    // Step 2: if result is (progn ...), recurse into subforms.
    // Root `val` during iteration: the recursive `eager_expand_eval`
    // call triggers evaluation + GC, which could free val's cons cells.
    if let Value::Cons(id) = val {
        let car = eval.heap.cons_car(id);
        let cdr = eval.heap.cons_cdr(id);
        if car.is_symbol_named("progn") {
            let saved_progn = eval.save_temp_roots();
            eval.push_temp_root(val);
            let mut result = Value::Nil;
            let mut tail = cdr;
            while let Value::Cons(sub_id) = tail {
                let sub_form = eval.heap.cons_car(sub_id);
                tail = eval.heap.cons_cdr(sub_id);
                result = eager_expand_eval(eval, sub_form, macroexpand_fn)?;
            }
            eval.restore_temp_roots(saved_progn);
            return Ok(result);
        }
    }

    // Step 3+4: full expand then eval —
    // val = eval_sub(macroexpand(val, t))
    // IMPORTANT: pass the already-one-level-expanded `val`, not the original
    // `form_value`.  Real Emacs (lread.c:2030) does:
    //   val = eval_sub(calln(macroexpand, val, Qt));
    let saved = eval.save_temp_roots();
    eval.push_temp_root(val);
    eval.push_temp_root(macroexpand_fn);
    let t3 = std::time::Instant::now();
    let fully_expanded = match eval.apply(macroexpand_fn, vec![val, Value::True]) {
        Ok(v) => v,
        Err(_) => {
            // Full expansion failed; use the one-level-expanded form.
            eval.restore_temp_roots(saved);
            tracing::debug!("eager_expand step3 failed, using partially expanded form");
            val
        }
    };
    let d3 = t3.elapsed();
    if d3.as_millis() > 200 {
        tracing::warn!("eager_expand step3 (full-expand) took {d3:.2?}");
    }
    eval.restore_temp_roots(saved);

    let saved = eval.save_temp_roots();
    eval.push_temp_root(fully_expanded);
    let t4 = std::time::Instant::now();
    let result = eval.eval_value(&fully_expanded).map_err(map_flow)?;
    let d4 = t4.elapsed();
    if d4.as_millis() > 200 {
        tracing::warn!("eager_expand step4 (eval) took {d4:.2?}");
    }
    eval.restore_temp_roots(saved);

    Ok(result)
}

/// Load and evaluate a file. Returns the last result.
#[tracing::instrument(level = "info", skip(eval), err(Debug))]
pub fn load_file(eval: &mut super::eval::Evaluator, path: &Path) -> Result<Value, EvalError> {
    if is_unsupported_compiled_path(path) {
        return Err(EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "Loading compiled Elisp artifacts (.elc/.elc.gz) is unsupported in neomacs. Rebuild from source and load the .el file: {}",
                path.display()
            ))],
        });
    }

    // Check for recursive load (mirrors lread.c:1202-1220).
    //
    // Official Emacs allows up to 3 recursive loads (erroring on the 4th).
    // However, NeoVM always loads .el source files, not .elc bytecode.
    // This matters because macro expansion of .el forms (e.g. define-inline,
    // cl-defstruct) triggers eager macroexpand-all, which can recursively
    // load the same file.  With .elc, pre-compiled bodies never trigger
    // re-loads.
    //
    // To match effective .elc behaviour: if a file is already being loaded,
    // silently skip the recursive load.  This prevents infinite recursion
    // from eager expansion of compile-time constructs like define-inline.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let load_count = eval
        .loads_in_progress
        .iter()
        .filter(|p| **p == canonical)
        .count();
    if load_count > 0 {
        tracing::debug!(
            "Skipping recursive load of {} (already in progress)",
            path.display()
        );
        return Ok(Value::True);
    }
    eval.loads_in_progress.push(canonical);

    let result = load_file_body(eval, path);

    eval.loads_in_progress.pop();
    result
}

fn load_file_body(
    eval: &mut super::eval::Evaluator,
    path: &Path,
) -> Result<Value, EvalError> {
    // Read raw bytes and decode with Emacs-extended UTF-8.  Some Lisp files
    // (e.g., ethiopic.el, tibetan.el) declare `coding: utf-8-emacs` and
    // contain byte sequences for code points above U+10FFFF (Emacs-internal
    // characters).  decode_emacs_utf8 handles these by converting `?<ext>`
    // character literals to `?\x<HEX>` escape syntax.
    let raw_bytes = std::fs::read(path).map_err(|e| EvalError::Signal {
        symbol: intern("file-error"),
        data: vec![Value::string(format!(
            "Cannot read file: {}: {}",
            path.display(),
            e
        ))],
    })?;
    let content = decode_emacs_utf8(&raw_bytes);

    // Save dynamic loader context and restore it even on parse/eval errors.
    let old_lexical = eval.lexical_binding();
    let old_load_file = eval.obarray().symbol_value("load-file-name").cloned();

    // Root the saved load-file-name value: it sits in a Rust local variable
    // that the GC cannot see.  Without this, nested load (require inside
    // a loaded file) can trigger gc_safe_point which sweeps the string,
    // causing stale ObjId panics when we try to restore it later.
    let saved_roots = eval.save_temp_roots();
    if let Some(ref v) = old_load_file {
        eval.push_temp_root(*v);
    }

    // Check for lexical-binding file variable in file-local line.
    if lexical_binding_enabled_for_source(&content) {
        eval.set_lexical_binding(true);
    }

    eval.set_variable(
        "load-file-name",
        Value::string(path.to_string_lossy().to_string()),
    );

    let result = (|| -> Result<Value, EvalError> {
        let forms = parse_source_with_cache(path, &content, eval.lexical_binding())?;

        // Clear the macro expansion cache to avoid stale entries from
        // previous files whose parsed form memory has been freed and
        // potentially reused at the same addresses.  Lambda-body caches
        // (Rc<Vec<Expr>>) are still valid but will be re-populated on
        // first use — a small one-time cost per file.
        eval.macro_expansion_cache.clear();

        // Eager macro expansion guard (like real Emacs's lread.c).
        // We need BOTH internal-macroexpand-for-load AND the pcase `
        // macroexpander to be defined, since macroexpand-all uses pcase
        // backquote patterns in its body.
        let macroexpand_fn: Option<Value> = get_eager_macroexpand_fn(eval);

        let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        for (i, form) in forms.iter().enumerate() {
            tracing::debug!(
                "{} FORM[{i}/{}]: {}",
                file_name,
                forms.len(),
                print_expr(form).chars().take(100).collect::<String>()
            );
            let start = std::time::Instant::now();
            let (h0, m0) = (eval.macro_cache_hits, eval.macro_cache_misses);
            let eval_result = if let Some(mexp_fn) = macroexpand_fn {
                let form_value = quote_to_value(form);
                eager_expand_eval(eval, form_value, mexp_fn)
            } else {
                eval.eval_expr(form)
            };
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
                // Build a human-readable error message resolving Str ObjIds
                let err_detail = match e {
                    EvalError::Signal { symbol, data } => {
                        let sym_name = super::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> = data.iter().map(|v| {
                            format_value_for_error(v)
                        }).collect();
                        format!("({} {})", sym_name, data_strs.join(" "))
                    }
                    other => format!("{:?}", other),
                };
                tracing::error!(
                    "  !! {file_name} FORM[{i}] FAILED: {} => {}",
                    print_expr(form).chars().take(120).collect::<String>(),
                    err_detail
                );
            }
            eval_result?;
            eval.gc_safe_point();

            // Note: we intentionally do NOT re-check `macroexpand_fn`
            // mid-file.  Enabling eager expansion mid-file breaks pcase.el
            // loading: once `\`--pcase-macroexpander` is defined, the re-check
            // would enable eager expansion for `pcase--expand-\``, but
            // macroexpand-all needs that very function (circular dependency).
            // Real Emacs prevents this via `macroexp--pending-eager-loads`;
            // we simply check once at file start and keep that mode for the
            // whole file.
        }

        record_load_history(eval, path);

        // Emacs `load` returns non-nil on success (typically `t`).
        Ok(Value::True)
    })();

    eval.set_lexical_binding(old_lexical);
    if let Some(old) = old_load_file {
        eval.set_variable("load-file-name", old);
    } else {
        eval.set_variable("load-file-name", Value::Nil);
    }
    eval.restore_temp_roots(saved_roots);

    result
}

fn record_load_history(eval: &mut super::eval::Evaluator, path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    let entry = Value::cons(Value::string(path_str), Value::Nil);
    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::Nil);
    eval.set_variable("load-history", Value::cons(entry, history));
}

/// Register bootstrap variables owned by the file-loading subsystem.
pub fn register_bootstrap_vars(obarray: &mut super::symbol::Obarray) {
    obarray.set_symbol_value("after-load-alist", Value::Nil);
    obarray.set_symbol_value("macroexp--dynvars", Value::Nil);
}

/// Create an Evaluator with the full Emacs bootstrap loaded (like GNU
/// Emacs's dumped state).  Mirrors the loadup.el boot sequence.
pub fn create_bootstrap_evaluator() -> Result<super::eval::Evaluator, EvalError> {
    // Discover the project root (contains lisp/ directory).
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("project root");
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );

    let mut eval = super::eval::Evaluator::new();

    // Set up load-path with lisp/ and its subdirectories.
    let subdirs = [
        "", "emacs-lisp", "progmodes", "language", "international",
        "textmodes", "vc", "leim",
    ];
    let mut load_path_entries = Vec::new();
    for sub in &subdirs {
        let dir = if sub.is_empty() {
            lisp_dir.clone()
        } else {
            lisp_dir.join(sub)
        };
        if dir.is_dir() {
            load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
        }
    }
    eval.set_variable("load-path", Value::list(load_path_entries));
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);
    eval.set_variable("max-lisp-eval-depth", Value::Int(4200));
    eval.set_variable("inhibit-load-charset-map", Value::True);
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
    eval.set_variable("exec-suffixes", Value::Nil);
    eval.set_variable("exec-directory", Value::Nil);

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

    // Suppress eager macro expansion during the bootstrap phase
    // (mirrors real Emacs loadup.el which wraps pcase loading with
    // `(let ((macroexp--pending-eager-loads '(skip))) ...)`.
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    // The files loadup.el loads, in order (excluding conditional
    // blocks we can't satisfy yet).
    let files = [
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "keymap",
        "version",
        "widget",
        "custom",
        "emacs-lisp/map-ynp",
        "international/mule",
        "international/mule-conf",
        "env",
        "format",
        "bindings",
        "window",
        "files",
        "emacs-lisp/macroexp",
        "emacs-lisp/pcase",
        "!enable-eager-expansion",
        "emacs-lisp/macroexp",  // Re-load
        "emacs-lisp/inline",
        "cus-face",
        "faces",
        "!bootstrap-cl-preloaded-stubs",
        "!require-gv",
        "!load-ldefs-boot",
        "button",
        "emacs-lisp/cl-preloaded",
        "emacs-lisp/oclosure",
        "obarray",
        "abbrev",
        "help",
        "jka-cmpr-hook",
        "epa-hook",
        "international/mule-cmds",
        "case-table",
        "international/characters",
        "composite",
        "language/chinese",
        "language/cyrillic",
        "language/indian",
        "language/sinhala",
        "language/english",
        "language/ethiopic",
        "language/european",
        "language/czech",
        "language/slovak",
        "language/romanian",
        "language/greek",
        "language/hebrew",
        "international/cp51932",
        "international/eucjp-ms",
        "language/japanese",
        "language/korean",
        "language/lao",
        "language/tai-viet",
        "language/thai",
        "language/tibetan",
        "language/vietnamese",
        "language/misc-lang",
        "language/utf-8-lang",
        "language/georgian",
        "language/khmer",
        "language/burmese",
        "language/cham",
        "language/philippine",
        "language/indonesian",
        "indent",
        "emacs-lisp/cl-generic",
        "simple",
        "emacs-lisp/seq",
        "emacs-lisp/nadvice",
        "emacs-lisp/cl-lib",
        "minibuffer",
        "frame",
        "startup",
        "term/tty-colors",
        "font-core",
        "emacs-lisp/syntax",
        "font-lock",
        "jit-lock",
        "mouse",
        "select",
        "emacs-lisp/timer",
        "emacs-lisp/easymenu",
        "isearch",
        "rfn-eshadow",
        "menu-bar",
        "tab-bar",
        "emacs-lisp/lisp",
        "textmodes/page",
        "register",
        "textmodes/paragraphs",
        "progmodes/prog-mode",
        "emacs-lisp/rx",
        "emacs-lisp/lisp-mode",
        "textmodes/text-mode",
        "textmodes/fill",
        "newcomment",
        "replace",
        "emacs-lisp/tabulated-list",
        "buff-menu",
        "fringe",
        "emacs-lisp/regexp-opt",
        "image",
        "international/fontset",
        "dnd",
        "tool-bar",
        "progmodes/elisp-mode",
        "emacs-lisp/float-sup",
        "vc/vc-hooks",
        "vc/ediff-hook",
        "uniquify",
        "electric",
        "paren",
        "emacs-lisp/shorthands",
        "emacs-lisp/eldoc",
        "emacs-lisp/cconv",
        "tooltip",
        "international/iso-transl",
        "emacs-lisp/rmc",
    ];

    let load_path = get_load_path(&eval.obarray());
    let total_files = files.len();

    for (file_idx, name) in files.iter().enumerate() {
        // Handle sentinel that enables eager expansion.
        if *name == "!enable-eager-expansion" {
            eval.set_variable("macroexp--pending-eager-loads", Value::Nil);
            tracing::info!("--- eager macro expansion ENABLED ---");
            continue;
        }
        // Handle sentinel for loading ldefs-boot.el (autoload definitions).
        if *name == "!load-ldefs-boot" {
            let ldefs_path = lisp_dir.join("ldefs-boot.el");
            if ldefs_path.exists() {
                tracing::info!("LOADING: ldefs-boot.el ...");
                let start = std::time::Instant::now();
                match load_file(&mut eval, &ldefs_path) {
                    Ok(_) => {
                        tracing::info!("  OK: ldefs-boot.el ({:.2?})", start.elapsed());
                    }
                    Err(e) => {
                        let msg = format!("{e:?}");
                        tracing::error!("FAIL: ldefs-boot.el => {msg}");
                        return Err(e);
                    }
                }
            } else {
                tracing::warn!("SKIP: ldefs-boot.el (not found)");
            }
            continue;
        }
        // Pre-define minimal cl-preloaded stubs so cl-macs can load.
        if *name == "!bootstrap-cl-preloaded-stubs" {
            let stubs = [
                "(defmacro cl--find-class (type) `(get ,type 'cl--class))",
                "(defun cl--builtin-type-p (name) nil)",
                "(defun cl--struct-name-p (name) (and name (symbolp name) (not (keywordp name))))",
                "(defvar cl-struct-cl-structure-object-tags nil)",
                "(defvar cl--struct-default-parent nil)",
                "(defun cl-struct-define (name docstring parent type named slots children-sym tag print) (when children-sym (if (boundp children-sym) (add-to-list children-sym tag) (set children-sym (list tag)))))",
                "(defun cl--define-derived-type (name expander predicate &optional parents) nil)",
                "(defmacro cl-function (func) `(function ,func))",
            ];
            for stub in &stubs {
                match crate::emacs_core::parser::parse_forms(stub) {
                    Ok(forms) => {
                        let results = eval.eval_forms(&forms);
                        for r in &results {
                            if let Err(e) = r {
                                tracing::error!("bootstrap stub failed: {stub} => {e:?}");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("bootstrap stub parse failed: {stub} => {e:?}");
                    }
                }
            }
            tracing::info!("--- cl-preloaded bootstrap stubs defined ---");
            continue;
        }
        // Handle sentinel for (require 'gv) — mirrors loadup.el line 199.
        if *name == "!require-gv" {
            tracing::info!("LOADING: (require 'gv) ...");
            let start = std::time::Instant::now();
            match eval.require_value(Value::symbol("gv"), None, None) {
                Ok(_) => {
                    tracing::info!("  OK: (require 'gv) ({:.2?})", start.elapsed());
                }
                Err(e) => {
                    let msg = format!("{e:?}");
                    tracing::warn!("  WARN: (require 'gv) failed: {msg}");
                }
            }
            continue;
        }
        tracing::info!("[{}/{}] LOADING: {name} ...", file_idx + 1, total_files);
        let (h0, m0) = (eval.macro_cache_hits, eval.macro_cache_misses);
        let start = std::time::Instant::now();
        match find_file_in_load_path(name, &load_path) {
            Some(path) => match load_file(&mut eval, &path) {
                Ok(_) => {
                    let dh = eval.macro_cache_hits - h0;
                    let dm = eval.macro_cache_misses - m0;
                    tracing::info!("  OK: {name} ({:.2?}) [cache hit={dh} miss={dm}]", start.elapsed());
                }
                Err(e) => {
                    let msg = match &e {
                        EvalError::Signal { symbol, data } => {
                            let sym = super::intern::resolve_sym(*symbol);
                            let data_strs: Vec<String> =
                                data.iter().map(|v| format!("{v}")).collect();
                            format!("({sym} {})", data_strs.join(" "))
                        }
                        EvalError::UncaughtThrow { tag, value } => {
                            format!("(throw {tag} {value})")
                        }
                    };
                    tracing::error!("FAIL: {name} => {msg}");
                    return Err(e);
                }
            },
            None => {
                tracing::error!("SKIP: {name} (not found in load-path)");
                return Err(EvalError::Signal {
                    symbol: intern("error"),
                    data: vec![Value::string(format!(
                        "loadup bootstrap: file not found: {name}"
                    ))],
                });
            }
        }
    }

    tracing::info!("\n=== LOADUP BOOTSTRAP COMPLETE ===");
    Ok(eval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::intern::resolve_sym;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CacheWriteFailGuard;

    impl CacheWriteFailGuard {
        fn set(phase: u8) -> Self {
            set_cache_write_fail_phase_for_test(phase);
            Self
        }
    }

    impl Drop for CacheWriteFailGuard {
        fn drop(&mut self) {
            clear_cache_write_fail_phase_for_test();
        }
    }

    #[test]
    fn cache_write_disable_env_value_matrix() {
        for value in ["1", "true", "TRUE", " yes ", "On", "\tyEs\n"] {
            assert!(
                cache_write_disabled_env_value(value),
                "expected '{value}' to disable load cache writes",
            );
        }

        for value in ["0", "false", "FALSE", "no", "off", "", "   ", "maybe"] {
            assert!(
                !cache_write_disabled_env_value(value),
                "expected '{value}' to leave load cache writes enabled",
            );
        }
    }

    #[test]
    fn strip_reader_prefix_handles_bom_and_shebang() {
        let source = "#!/usr/bin/env emacs --script\n(setq vm-shebang-strip 1)\n";
        assert_eq!(
            strip_reader_prefix(source),
            ("(setq vm-shebang-strip 1)\n", false),
            "shebang-prefixed source should drop the first line before parsing",
        );
        assert_eq!(
            strip_reader_prefix("#!/usr/bin/env emacs --script"),
            ("", true),
            "single-line shebang files should preserve end-of-file signaling",
        );
        assert_eq!(
            strip_reader_prefix("(setq vm-shebang-strip 2)\n"),
            ("(setq vm-shebang-strip 2)\n", false),
            "non-shebang source should remain unchanged",
        );
        assert_eq!(
            strip_reader_prefix("\u{feff}(setq vm-bom-strip 3)\n"),
            ("(setq vm-bom-strip 3)\n", false),
            "utf-8 bom should be removed before parsing",
        );
        assert_eq!(
            strip_reader_prefix("\u{feff}#!/usr/bin/env emacs --script\n(setq vm-bom-shebang 4)\n"),
            ("(setq vm-bom-shebang 4)\n", false),
            "utf-8 bom should not block shebang stripping",
        );
    }

    #[test]
    fn lexical_binding_detects_second_line_cookie_after_shebang() {
        assert!(
            lexical_binding_enabled_in_file_local_cookie_line(
                ";; -*- mode: emacs-lisp; lexical-binding: t; -*-",
            ),
            "lexical-binding cookie should be parsed from -*- metadata block",
        );
        assert!(
            !lexical_binding_enabled_in_file_local_cookie_line(
                "(setq vm-lb-false \"lexical-binding: t\")",
            ),
            "plain source text must not be treated as file-local cookie metadata",
        );
        assert!(
            !lexical_binding_enabled_in_file_local_cookie_line(";; -*- Lexical-Binding: t; -*-",),
            "cookie keys are case-sensitive in oracle behavior",
        );
        assert!(
            lexical_binding_enabled_for_source(
                "#!/usr/bin/env emacs --script\n;; -*- lexical-binding: t; -*-\n(setq vm-lb 1)\n",
            ),
            "second-line lexical-binding cookie should be honored for shebang scripts",
        );
        assert!(
            !lexical_binding_enabled_for_source(
                ";; no cookie on first line\n;; -*- lexical-binding: t; -*-\n",
            ),
            "second-line cookie should not activate lexical binding without shebang",
        );
    }

    #[test]
    fn find_file_nonexistent() {
        assert!(find_file_in_load_path("nonexistent", &[]).is_none());
    }

    #[test]
    fn load_path_extraction() {
        let mut ob = super::super::symbol::Obarray::new();
        ob.set_symbol_value("default-directory", Value::string("/tmp/project"));
        ob.set_symbol_value(
            "load-path",
            Value::list(vec![
                Value::string("/usr/share/emacs/lisp"),
                Value::Nil,
                Value::string("/home/user/.emacs.d"),
            ]),
        );
        let paths = get_load_path(&ob);
        assert_eq!(
            paths,
            vec![
                "/usr/share/emacs/lisp",
                "/tmp/project",
                "/home/user/.emacs.d"
            ]
        );
    }

    #[test]
    fn find_file_with_suffix_flags() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-flags-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");

        let plain = dir.join("choice");
        let el = dir.join("choice.el");
        fs::write(&plain, "plain").expect("write plain fixture");
        fs::write(&el, "el").expect("write el fixture");

        let load_path = vec![dir.to_string_lossy().to_string()];

        // Default mode prefers suffixed files.
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
            Some(el.clone())
        );
        // no-suffix mode only tries exact name.
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, true, false, false),
            Some(plain.clone())
        );
        // must-suffix mode rejects plain file and requires suffixed one.
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, false, true, false),
            Some(el)
        );
        // no-suffix takes precedence if both flags are set.
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, true, true, false),
            Some(plain)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_file_prefers_earlier_load_path_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("neovm-load-path-order-{unique}"));
        let d1 = root.join("d1");
        let d2 = root.join("d2");
        fs::create_dir_all(&d1).expect("create d1");
        fs::create_dir_all(&d2).expect("create d2");

        let plain = d1.join("choice");
        let el = d2.join("choice.el");
        fs::write(&plain, "plain").expect("write plain fixture");
        fs::write(&el, "el").expect("write el fixture");

        let load_path = vec![
            d1.to_string_lossy().to_string(),
            d2.to_string_lossy().to_string(),
        ];
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
            Some(plain)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_file_prefers_newer_source_when_enabled() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-prefer-newer-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");

        let elc = dir.join("choice.elc");
        let el = dir.join("choice.el");
        fs::write(&elc, "compiled").expect("write compiled fixture");
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&el, "source").expect("write source fixture");

        let load_path = vec![dir.to_string_lossy().to_string()];
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
            Some(el.clone())
        );
        assert_eq!(
            find_file_in_load_path_with_flags("choice", &load_path, false, false, true),
            Some(el)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_records_load_history() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-history-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(&file, "(setq vm-load-history-probe t)\n").expect("write fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load file");
        assert_eq!(loaded, Value::True);

        let history = eval
            .obarray()
            .symbol_value("load-history")
            .cloned()
            .unwrap_or(Value::Nil);
        let entries = super::super::value::list_to_vec(&history).expect("load-history is a list");
        assert!(
            !entries.is_empty(),
            "load-history should have at least one entry"
        );
        let first = super::super::value::list_to_vec(&entries[0]).expect("entry is a list");
        let path_str = file.to_string_lossy().to_string();
        assert_eq!(
            first.first().and_then(Value::as_str),
            Some(path_str.as_str())
        );
        assert_eq!(
            eval.obarray().symbol_value("load-file-name").cloned(),
            Some(Value::Nil)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_load_restores_parent_load_file_name() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-file-name-nested-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let parent = dir.join("parent.el");
        let child = dir.join("child.el");

        fs::write(
            &parent,
            "(setq vm-parent-seen load-file-name)\n\
             (load (expand-file-name \"child\" (file-name-directory load-file-name)) nil 'nomessage)\n\
             (setq vm-parent-after-child load-file-name)\n",
        )
        .expect("write parent fixture");
        fs::write(&child, "(setq vm-child-seen load-file-name)\n").expect("write child fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &parent).expect("load parent fixture");
        assert_eq!(loaded, Value::True);

        let parent_str = parent.to_string_lossy().to_string();
        let child_str = child.to_string_lossy().to_string();
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-parent-seen")
                .and_then(Value::as_str),
            Some(parent_str.as_str())
        );
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-child-seen")
                .and_then(Value::as_str),
            Some(child_str.as_str())
        );
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-parent-after-child")
                .and_then(Value::as_str),
            Some(parent_str.as_str())
        );
        assert_eq!(
            eval.obarray().symbol_value("load-file-name").cloned(),
            Some(Value::Nil),
            "load-file-name should be restored after top-level load",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_accepts_shebang_and_honors_second_line_lexical_binding_cookie() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-shebang-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(
            &file,
            "#!/usr/bin/env emacs --script\n\
             ;; -*- lexical-binding: t; -*-\n\
             (setq vm-load-shebang-probe lexical-binding)\n\
             (setq vm-load-shebang-fn (let ((x 41)) (lambda () (+ x 1))))\n",
        )
        .expect("write shebang fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load shebang fixture");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-load-shebang-probe")
                .cloned(),
            Some(Value::True),
            "second-line lexical-binding cookie should set lexical-binding to t during load",
        );

        let call = super::super::parser::parse_forms(
            "(let ((lexical-binding nil)) (funcall vm-load-shebang-fn))",
        )
        .expect("parse call fixture");
        let value = eval.eval_expr(&call[0]).expect("evaluate closure");
        assert_eq!(
            value.as_int(),
            Some(42),
            "closure should capture lexical scope from loaded file",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_does_not_enable_lexical_binding_from_non_cookie_second_line_text() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-shebang-noncookie-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(
            &file,
            "#!/usr/bin/env emacs --script\n\
             (setq vm-load-shebang-false-string \"lexical-binding: t\")\n\
             (setq vm-load-shebang-false-probe lexical-binding)\n\
             (setq vm-load-shebang-false-fn (let ((x 41)) (lambda () (+ x 1))))\n",
        )
        .expect("write shebang non-cookie fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load shebang non-cookie fixture");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-load-shebang-false-probe")
                .cloned(),
            Some(Value::Nil),
            "non-cookie second-line text must not flip lexical-binding to t",
        );

        let call = super::super::parser::parse_forms(
            "(condition-case err (let ((lexical-binding nil)) (funcall vm-load-shebang-false-fn)) (error (list 'error (car err))))",
        )
        .expect("parse call fixture");
        let value = eval
            .eval_expr(&call[0])
            .expect("evaluate closure failure probe");
        let payload =
            super::super::value::list_to_vec(&value).expect("expected error payload list");
        assert_eq!(
            payload,
            vec![Value::symbol("error"), Value::symbol("void-variable")],
            "without lexical-binding cookie, closure must not capture lexical locals",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_accepts_utf8_bom_prefixed_source() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-bom-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(
            &file,
            "\u{feff}(setq vm-load-bom-probe 'ok)\n(setq vm-load-bom-flag t)\n",
        )
        .expect("write bom fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load bom fixture");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray().symbol_value("vm-load-bom-probe").cloned(),
            Some(Value::symbol("ok")),
            "utf-8 bom should be ignored by reader before first form",
        );
        assert_eq!(
            eval.obarray().symbol_value("vm-load-bom-flag").cloned(),
            Some(Value::True)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_single_line_shebang_signals_end_of_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-shebang-eof-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(&file, "#!/usr/bin/env emacs --script").expect("write shebang-only fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let err = load_file(&mut eval, &file).expect_err("shebang-only source should signal EOF");
        match err {
            EvalError::Signal { symbol, data } => {
                assert_eq!(resolve_sym(symbol), "end-of-file");
                assert!(data.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_writes_and_invalidates_neoc_cache() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-neoc-cache-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        let source_v1 = "(setq vm-load-cache-probe 'v1)\n";
        fs::write(&file, source_v1).expect("write source fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load source file");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray().symbol_value("vm-load-cache-probe").cloned(),
            Some(Value::symbol("v1"))
        );

        let cache = cache_sidecar_path(&file);
        assert!(
            cache.exists(),
            "source load should create .neoc sidecar cache"
        );
        let cache_v1 = fs::read_to_string(&cache).expect("read cache v1");
        assert!(
            cache_v1.contains(&format!("key={}", cache_key(false))),
            "cache key should include lexical-binding dimension",
        );
        assert!(
            cache_v1.contains(&format!("source-hash={:016x}", source_hash(source_v1))),
            "cache should carry source hash invalidation key",
        );

        let source_v2 = ";;; -*- lexical-binding: t; -*-\n(setq vm-load-cache-probe 'v2)\n";
        fs::write(&file, source_v2).expect("write source fixture v2");

        let loaded = load_file(&mut eval, &file).expect("reload source file");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray().symbol_value("vm-load-cache-probe").cloned(),
            Some(Value::symbol("v2"))
        );
        let cache_v2 = fs::read_to_string(&cache).expect("read cache v2");
        assert_ne!(cache_v1, cache_v2, "cache must refresh when source changes");
        assert!(
            cache_v2.contains(&format!("key={}", cache_key(true))),
            "cache key should update when lexical-binding dimension changes",
        );
        assert!(
            cache_v2.contains(&format!("source-hash={:016x}", source_hash(source_v2))),
            "cache hash should update when source text changes",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_ignores_corrupt_neoc_cache_and_loads_source() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-neoc-corrupt-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(&file, "(setq vm-load-corrupt-neoc 'ok)\n").expect("write source fixture");
        let cache = cache_sidecar_path(&file);
        fs::write(&cache, "corrupt-neoc-cache").expect("write corrupt cache");

        let mut eval = super::super::eval::Evaluator::new();
        let loaded = load_file(&mut eval, &file).expect("load should ignore corrupt cache");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray().symbol_value("vm-load-corrupt-neoc").cloned(),
            Some(Value::symbol("ok"))
        );
        let rewritten = fs::read_to_string(&cache).expect("cache should be rewritten");
        assert!(
            rewritten.starts_with(ELISP_CACHE_MAGIC),
            "rewritten cache should have expected header",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_ignores_cache_write_failures_before_write() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-neoc-write-fail-pre-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(&file, "(setq vm-load-neoc-write-fail-pre 'ok)\n").expect("write source fixture");

        let _guard = CacheWriteFailGuard::set(CACHE_WRITE_PHASE_BEFORE_WRITE);
        let mut eval = super::super::eval::Evaluator::new();
        let loaded =
            load_file(&mut eval, &file).expect("load should succeed despite cache write failure");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-load-neoc-write-fail-pre")
                .cloned(),
            Some(Value::symbol("ok"))
        );
        assert!(
            !cache_sidecar_path(&file).exists(),
            "cache should be absent when write fails before cache file creation",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_cleans_tmp_after_cache_write_failure_before_rename() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-neoc-write-fail-post-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let file = dir.join("probe.el");
        fs::write(&file, "(setq vm-load-neoc-write-fail-post 'ok)\n")
            .expect("write source fixture");

        let _guard = CacheWriteFailGuard::set(CACHE_WRITE_PHASE_AFTER_WRITE);
        let mut eval = super::super::eval::Evaluator::new();
        let loaded =
            load_file(&mut eval, &file).expect("load should succeed despite cache rename failure");
        assert_eq!(loaded, Value::True);
        assert_eq!(
            eval.obarray()
                .symbol_value("vm-load-neoc-write-fail-post")
                .cloned(),
            Some(Value::symbol("ok"))
        );
        assert!(
            !cache_sidecar_path(&file).exists(),
            "cache should be absent when failure happens before rename",
        );
        assert!(
            !cache_temp_path(&file).exists(),
            "temporary cache file should be cleaned after write failure",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_elc_is_explicitly_unsupported() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-elc-unsupported-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let compiled = dir.join("probe.elc");
        fs::write(&compiled, "compiled-data").expect("write compiled fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc");
        match err {
            EvalError::Signal { symbol, data } => {
                assert_eq!(resolve_sym(symbol), "error");
                assert!(
                    data.iter().any(|v| v
                        .as_str()
                        .is_some_and(|s| s.contains("unsupported in neomacs"))),
                    "error should explain .elc policy",
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_elc_is_rejected_even_if_sibling_el_exists() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-elc-with-sibling-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let source = dir.join("probe.el");
        let compiled = dir.join("probe.elc");
        fs::write(&source, "(setq vm-load-elc-sibling 'source)\n").expect("write source fixture");
        fs::write(&compiled, "compiled-data").expect("write compiled fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc");
        match err {
            EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            eval.obarray().symbol_value("vm-load-elc-sibling").cloned(),
            None,
            "rejecting .elc should not implicitly load source sibling",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_file_surfaces_elc_only_artifact_as_explicit_unsupported_load_target() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-elc-only-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");

        let compiled = dir.join("module.elc");
        fs::write(&compiled, "compiled").expect("write compiled fixture");

        let load_path = vec![dir.to_string_lossy().to_string()];
        let found = find_file_in_load_path_with_flags("module", &load_path, false, false, false);
        assert_eq!(found, Some(compiled));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_elc_gz_is_explicitly_unsupported() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-load-elc-gz-unsupported-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let compiled = dir.join("probe.elc.gz");
        fs::write(&compiled, "compiled-data").expect("write compiled fixture");

        let mut eval = super::super::eval::Evaluator::new();
        let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc.gz");
        match err {
            EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn precompile_source_file_writes_deterministic_cache() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-precompile-deterministic-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let source = dir.join("probe.el");
        fs::write(
            &source,
            ";;; -*- lexical-binding: t; -*-\n(setq vm-precompile-probe '(1 2 3))\n",
        )
        .expect("write source fixture");

        let cache_path_1 =
            precompile_source_file(&source).expect("first precompile should succeed");
        let cache_v1 = fs::read_to_string(&cache_path_1).expect("read cache v1");
        let cache_path_2 =
            precompile_source_file(&source).expect("second precompile should succeed");
        let cache_v2 = fs::read_to_string(&cache_path_2).expect("read cache v2");

        assert_eq!(cache_path_1, cache_path_2, "cache path should be stable");
        assert_eq!(
            cache_v1, cache_v2,
            "precompile output should be deterministic"
        );
        assert!(
            cache_v1.contains("lexical=1"),
            "lexical-binding should be reflected in cache key",
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn precompile_source_file_rejects_compiled_inputs() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neovm-precompile-reject-elc-{unique}"));
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let compiled = dir.join("probe.elc");
        fs::write(&compiled, "compiled").expect("write compiled fixture");

        let err = precompile_source_file(&compiled).expect_err("elc input should be rejected");
        match err {
            EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "file-error"),
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    /// Try loading the full loadup.el file sequence through the NeoVM
    /// evaluator.  This test runs by default.  Set
    /// NEOVM_LOADUP_TEST_SKIP=1 to skip it.
    #[test]
    fn neovm_loadup_bootstrap() {
        if std::env::var("NEOVM_LOADUP_TEST_SKIP").as_deref() == Ok("1") {
            tracing::info!("skipping neovm_loadup_bootstrap (NEOVM_LOADUP_TEST_SKIP=1)");
            return;
        }

        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();

        let _eval = create_bootstrap_evaluator()
            .expect("loadup bootstrap should succeed");
    }

    /// Minimal test: load enough files to get macroexpand-all + pcase working,
    /// then try (macroexpand-all '(pcase x (1 "one") (2 "two"))) and see
    /// if it terminates.
    #[test]
    fn macroexpand_all_pcase_terminates() {
        if std::env::var("NEOVM_LOADUP_TEST").as_deref() != Ok("1") {
            tracing::info!("skipping (set NEOVM_LOADUP_TEST=1)");
            return;
        }
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let project_root = manifest.parent().and_then(|p| p.parent()).expect("root");
        let lisp_dir = project_root.join("lisp");
        assert!(lisp_dir.is_dir());
        let mut eval = crate::emacs_core::eval::Evaluator::new();
        let subdirs = ["", "emacs-lisp"];
        let mut load_path_entries = Vec::new();
        for sub in &subdirs {
            let dir = if sub.is_empty() { lisp_dir.clone() } else { lisp_dir.join(sub) };
            if dir.is_dir() {
                load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
            }
        }
        eval.set_variable("load-path", Value::list(load_path_entries));
        eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
        eval.set_variable("purify-flag", Value::Nil);
        eval.set_variable("max-lisp-eval-depth", Value::Int(4200));

        let load_path = get_load_path(&eval.obarray());
        let load_and_report = |eval: &mut crate::emacs_core::eval::Evaluator, name: &str, load_path: &[String]| {
            let path = find_file_in_load_path(name, load_path).expect(name);
            load_file(eval, &path).unwrap_or_else(|e| {
                let msg = match &e {
                    EvalError::Signal { symbol, data } => {
                        let sym = crate::emacs_core::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
                        format!("({sym} {})", data_strs.join(" "))
                    }
                    other => format!("{other:?}"),
                };
                panic!("Failed to load {name}: {msg}");
            });
            tracing::info!("  loaded: {name}");
        };
        // Load minimum set: debug-early, byte-run, backquote, subr, macroexp, pcase
        for name in &[
            "emacs-lisp/debug-early",
            "emacs-lisp/byte-run",
            "emacs-lisp/backquote",
            "subr",
        ] {
            load_and_report(&mut eval, name, &load_path);
        }
        // macroexp + pcase: loaded without eager expansion since
        // get_eager_macroexpand_fn requires both internal-macroexpand-for-load
        // AND `--pcase-macroexpander to be defined.
        load_and_report(&mut eval, "emacs-lisp/macroexp", &load_path);
        load_and_report(&mut eval, "emacs-lisp/pcase", &load_path);

        // Test eager expansion with a simple defun containing pcase
        tracing::debug!("Testing eager expansion on a simple defun with cond...");
        let test_form = "(defun test-eager (x) (cond ((= x 1) \"one\") ((= x 2) \"two\") (t \"other\")))";
        let form_expr = &crate::emacs_core::parser::parse_forms(test_form).unwrap()[0];
        let form_value = quote_to_value(form_expr);
        let mexp_fn = eval.obarray().symbol_function("internal-macroexpand-for-load").cloned();
        match mexp_fn {
            Some(mfn) => {
                tracing::debug!("  internal-macroexpand-for-load found: {mfn}");
                match eager_expand_eval(&mut eval, form_value, mfn) {
                    Ok(v) => tracing::debug!("  eager expand+eval OK: {v}"),
                    Err(e) => tracing::debug!("  eager expand+eval ERR: {e:?}"),
                }
            }
            None => tracing::debug!("  internal-macroexpand-for-load NOT FOUND"),
        }

        // Test with backquote pattern (like macroexp--expand-all uses)
        tracing::debug!("Testing eager expansion on pcase with backquote pattern...");
        let test_form2 = "(pcase '(cond (t 1)) (`(cond . ,clauses) clauses) (_ nil))";
        let form_expr2 = &crate::emacs_core::parser::parse_forms(test_form2).unwrap()[0];
        match eval.eval_expr(form_expr2) {
            Ok(v) => tracing::debug!("  pcase backquote OK: {v}"),
            Err(e) => tracing::debug!("  pcase backquote ERR: {e:?}"),
        }

        tracing::debug!("All macroexpand-all pcase tests completed");
    }

    #[test]
    fn key_parse_modifier_bits() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();

        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let project_root = manifest
            .parent()
            .and_then(|p| p.parent())
            .expect("project root");
        let lisp_dir = project_root.join("lisp");
        if !lisp_dir.is_dir() {
            tracing::info!("skipping key_parse_modifier_bits: no lisp/ directory");
            return;
        }

        let mut eval = crate::emacs_core::eval::Evaluator::new();

        // Set up load-path
        let subdirs = ["", "emacs-lisp"];
        let mut load_path_entries = Vec::new();
        for sub in &subdirs {
            let dir = if sub.is_empty() {
                lisp_dir.clone()
            } else {
                lisp_dir.join(sub)
            };
            if dir.is_dir() {
                load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
            }
        }
        eval.set_variable("load-path", Value::list(load_path_entries));
        eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
        eval.set_variable("purify-flag", Value::Nil);

        // Load the minimum bootstrap: debug-early, byte-run, backquote, subr, keymap
        let load_path = get_load_path(&eval.obarray());
        for name in &[
            "emacs-lisp/debug-early",
            "emacs-lisp/byte-run",
            "emacs-lisp/backquote",
            "subr",
            "keymap",
        ] {
            let path = find_file_in_load_path(name, &load_path)
                .unwrap_or_else(|| panic!("cannot find {name} in load-path"));
            load_file(&mut eval, &path)
                .unwrap_or_else(|e| panic!("failed to load {name}: {e:?}"));
        }

        // Test key-parse with various modifier keys
        let test_cases = [
            // key-parse tests
            ("(key-parse \"C-M-q\")", "key-parse C-M-q"),
            // keymap-set with key string
            ("(let ((map (make-sparse-keymap))) (keymap-set map \"C-M-q\" #'ignore) map)", "keymap-set C-M-q"),
            // defvar-keymap
            ("(defvar-keymap test-prog-mode-map :doc \"test\" \"C-M-q\" #'ignore \"M-q\" #'ignore)", "defvar-keymap"),
        ];

        for (expr_str, desc) in &test_cases {
            let forms = super::super::parser::parse_forms(expr_str)
                .unwrap_or_else(|e| panic!("parse error for {expr_str}: {e:?}"));
            match eval.eval_expr(&forms[0]) {
                Ok(val) => tracing::debug!("  OK: {desc}: {expr_str} => {val}"),
                Err(e) => {
                    let msg = match &e {
                        EvalError::Signal { symbol, data } => {
                            let sym = super::super::intern::resolve_sym(*symbol);
                            let data_strs: Vec<String> =
                                data.iter().map(|v| format!("{v}")).collect();
                            format!("({sym} {})", data_strs.join(" "))
                        }
                        EvalError::UncaughtThrow { tag, value } => {
                            format!("(throw {tag} {value})")
                        }
                    };
                    tracing::error!("FAIL: {desc}: {expr_str} => {msg}");
                }
            }
        }

        // The critical test: key-parse "C-x" should succeed (not error)
        let forms = super::super::parser::parse_forms("(key-parse \"C-x\")")
            .expect("parse key-parse call");
        let result = eval.eval_expr(&forms[0]);
        match &result {
            Err(EvalError::Signal { symbol, data }) => {
                let sym = super::super::intern::resolve_sym(*symbol);
                let data_strs: Vec<String> =
                    data.iter().map(|v| format!("{v}")).collect();
                panic!("key-parse \"C-x\" failed: ({sym} {})", data_strs.join(" "));
            }
            Err(e) => panic!("key-parse \"C-x\" failed: {e:?}"),
            Ok(val) => tracing::debug!("key-parse \"C-x\" => {val}"),
        }
    }
}
