//! File loading and module system (require/provide/load).

use super::error::{EvalError, map_flow};
use super::eval::{quote_to_value, value_to_expr};
use super::expr::Expr;
use super::expr::print_expr;
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
            super::value::with_heap(|h: &crate::gc::LispHeap| format!("\"{}\"", h.get_string(*id)))
        }
        Value::Int(n) => format!("{}", n),
        Value::Char(c) => format!("?{}", c),
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        Value::Cons(id) => super::value::with_heap(|h: &crate::gc::LispHeap| {
            let car = h.cons_car(*id);
            let cdr = h.cons_cdr(*id);
            let car_s = format_value_for_error(&car);
            let cdr_s = format_value_for_error(&cdr);
            if cdr == Value::Nil {
                format!("({})", car_s)
            } else {
                format!("({} . {})", car_s, cdr_s)
            }
        }),
        other => format!("{:?}", other),
    }
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

/// Check if NEOVM_PREFER_ELC environment variable is set.
fn prefer_elc() -> bool {
    std::env::var("NEOVM_PREFER_ELC").is_ok()
}

fn pick_suffixed(base: &Path, _prefer_newer: bool) -> Option<PathBuf> {
    let el = source_suffixed_path(base);
    let elc = compiled_suffixed_path(base);

    if prefer_elc() {
        // Prefer .elc over .el
        if elc.exists() {
            return Some(elc);
        }
        if el.exists() {
            return Some(el);
        }
    } else {
        // Default: prefer .el over .elc
        if el.exists() {
            return Some(el);
        }
        if elc.exists() {
            return Some(elc);
        }
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

const ELISP_EXPANDED_CACHE_MAGIC: &str = "NEOVM-ELISP-CACHE-V2";
const ELISP_EXPANDED_CACHE_SCHEMA: &str = "schema=2";

fn expanded_cache_key(lexical_binding: bool) -> String {
    let lexical = if lexical_binding { "1" } else { "0" };
    format!("{ELISP_EXPANDED_CACHE_SCHEMA};vm={ELISP_CACHE_VM_VERSION};lexical={lexical}")
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

fn maybe_load_expanded_cache(
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

    if magic != ELISP_EXPANDED_CACHE_MAGIC {
        return None;
    }
    if !blank.is_empty() {
        return None;
    }

    let expected_key = format!("key={}", expanded_cache_key(lexical_binding));
    if key != expected_key {
        return None;
    }
    let expected_hash = format!("source-hash={:016x}", source_hash(source));
    if hash != expected_hash {
        return None;
    }

    super::parser::parse_forms(payload).ok()
}

fn write_expanded_cache(
    source_path: &Path,
    source: &str,
    lexical_binding: bool,
    forms: &[Expr],
) -> std::io::Result<()> {
    let cache_path = cache_sidecar_path(source_path);
    let payload = forms.iter().map(print_expr).collect::<Vec<_>>().join("\n");
    let raw = format!(
        "{ELISP_EXPANDED_CACHE_MAGIC}\nkey={}\nsource-hash={:016x}\n\n{}\n",
        expanded_cache_key(lexical_binding),
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
    // .elc is now supported; only block compressed .elc.gz
    name.ends_with(".elc.gz")
}

/// Parse and precompile a source `.el` file into a `.neoc` sidecar cache.
///
/// The emitted cache is an internal NeoVM artifact and not a compatibility
/// boundary. Failures to persist cache are reported to callers.
pub fn precompile_source_file(source_path: &Path) -> Result<PathBuf, EvalError> {
    // Reject both .elc and .elc.gz for precompilation (it only operates on .el sources).
    let is_compiled = source_path.extension().and_then(|e| e.to_str()) == Some("elc")
        || is_unsupported_compiled_path(source_path);
    if is_compiled {
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

/// Check if a Value is a `(quote ...)` form.
fn is_quote_form(val: Value, heap: &crate::gc::heap::LispHeap) -> bool {
    if let Value::Cons(id) = val {
        heap.cons_car(id).is_symbol_named("quote")
    } else {
        false
    }
}

/// Like `eager_expand_eval`, but also collects fully-expanded forms into a
/// `Vec<Expr>` for V2 cache serialization. The expanded Expr is captured
/// BEFORE eval (while the heap value is still rooted and stable).
///
/// **`eval-and-compile` handling**: Macros like `eval-and-compile` evaluate
/// their body at expansion time and return `(quote RESULT)`. The side effects
/// (e.g. type registration via `oclosure--define`) happen during expansion
/// and are lost in the quoted result. To preserve these side effects for V2
/// replay, we detect when expansion collapses a non-quote form into a quote
/// and cache the ORIGINAL form instead. On V2 replay, the evaluator handles
/// `eval-and-compile` as a special form, re-executing the body and its side
/// effects.
fn eager_expand_eval_and_collect(
    eval: &mut super::eval::Evaluator,
    form_value: Value,
    macroexpand_fn: Value,
    collector: &mut Vec<Expr>,
) -> Result<Value, EvalError> {
    // Step 1: one-level expand
    let saved = eval.save_temp_roots();
    eval.push_temp_root(form_value);
    eval.push_temp_root(macroexpand_fn);
    let val = match eval.apply(macroexpand_fn, vec![form_value, Value::Nil]) {
        Ok(v) => v,
        Err(_) => {
            // Expansion failed — collect original form, fall back to plain eval
            eval.restore_temp_roots(saved);
            tracing::debug!("eager_expand_collect step1 failed, falling back to plain eval");
            collector.push(value_to_expr(&form_value));
            return eval.eval_value(&form_value).map_err(map_flow);
        }
    };
    eval.restore_temp_roots(saved);

    // Detect expansion-time side-effect loss: if one-level expand turned a
    // non-quote form into (quote ...), the macro (e.g., eval-and-compile)
    // evaluated its body during expansion. Cache the ORIGINAL form so that
    // side effects re-occur on V2 replay.
    let use_original_for_cache =
        is_quote_form(val, &eval.heap) && !is_quote_form(form_value, &eval.heap);

    // Step 2: if result is (progn ...), recurse into subforms (flattens into collector)
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
                result = eager_expand_eval_and_collect(eval, sub_form, macroexpand_fn, collector)?;
            }
            eval.restore_temp_roots(saved_progn);
            return Ok(result);
        }
    }

    // Step 3: full expand
    let saved = eval.save_temp_roots();
    eval.push_temp_root(val);
    eval.push_temp_root(macroexpand_fn);
    let fully_expanded = match eval.apply(macroexpand_fn, vec![val, Value::True]) {
        Ok(v) => v,
        Err(_) => {
            eval.restore_temp_roots(saved);
            tracing::debug!("eager_expand_collect step3 failed, using partially expanded form");
            val
        }
    };
    eval.restore_temp_roots(saved);

    // Step 4: capture Expr BEFORE eval, then eval.
    // If the macro had expansion-time side effects (use_original_for_cache),
    // cache the original form so those side effects re-run on V2 replay.
    let saved = eval.save_temp_roots();
    eval.push_temp_root(fully_expanded);
    if use_original_for_cache {
        collector.push(value_to_expr(&form_value));
    } else {
        collector.push(value_to_expr(&fully_expanded));
    }
    let result = eval.eval_value(&fully_expanded).map_err(map_flow)?;
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
                "Loading compressed compiled Elisp artifacts (.elc.gz) is unsupported in neomacs: {}",
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

fn load_file_body(eval: &mut super::eval::Evaluator, path: &Path) -> Result<Value, EvalError> {
    // Check for .elc file and use the compiled loading path.
    if path.extension().and_then(|e| e.to_str()) == Some("elc") {
        return load_elc_file_body(eval, path);
    }

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

        // === V2 expanded-cache fast path ===
        // If macroexpand is available AND we have a V2 cache hit, just eval
        // the pre-expanded forms directly — no macro expansion needed.
        if macroexpand_fn.is_some() {
            if let Some(expanded_forms) =
                maybe_load_expanded_cache(path, &content, eval.lexical_binding())
            {
                tracing::info!(
                    "V2 cache hit for {} ({} forms)",
                    path.display(),
                    expanded_forms.len()
                );
                for form in &expanded_forms {
                    eval.eval_expr(form)?;
                    eval.gc_safe_point();
                }
                record_load_history(eval, path);
                return Ok(Value::True);
            }
        }

        // === V1 parse cache or fresh parse ===
        let forms = parse_source_with_cache(path, &content, eval.lexical_binding())?;

        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Collector for V2 cache: only populated when macroexpand is available.
        let mut expanded_collector: Vec<Expr> = if macroexpand_fn.is_some() {
            Vec::with_capacity(forms.len())
        } else {
            Vec::new()
        };

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
                eager_expand_eval_and_collect(eval, form_value, mexp_fn, &mut expanded_collector)
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
                        let data_strs: Vec<String> =
                            data.iter().map(|v| format_value_for_error(v)).collect();
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

        // Write V2 expanded cache if we collected forms and none contain OpaqueValues.
        if !expanded_collector.is_empty()
            && !expanded_collector.iter().any(|e| e.contains_opaque_value())
            && load_cache_writes_enabled()
        {
            match write_expanded_cache(path, &content, eval.lexical_binding(), &expanded_collector)
            {
                Ok(()) => tracing::info!(
                    "V2 cache written for {} ({} expanded forms)",
                    path.display(),
                    expanded_collector.len()
                ),
                Err(e) => tracing::debug!("V2 cache write failed for {}: {e}", path.display()),
            }
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

/// Load a `.elc` (GNU Emacs byte-compiled) file.
///
/// `.elc` files contain:
/// 1. A magic header: `;ELC` followed by version bytes and comment lines
/// 2. A `coding:` cookie in the comments (usually `utf-8-emacs-unix`)
/// 3. Top-level S-expressions, typically `(byte-code ...)` forms
///
/// The parser already handles `.elc`-specific reader syntax:
/// - `#[...]` → `(byte-code-literal VECTOR)` for compiled function objects
/// - `#@N<bytes>` → reader skip for inline docstring data blocks
/// - `#$` → `load-file-name` symbol reference
fn load_elc_file_body(eval: &mut super::eval::Evaluator, path: &Path) -> Result<Value, EvalError> {
    let raw_bytes = std::fs::read(path).map_err(|e| EvalError::Signal {
        symbol: intern("file-error"),
        data: vec![Value::string(format!(
            "Cannot read file: {}: {}",
            path.display(),
            e
        ))],
    })?;

    // Skip the ;ELC magic header.
    // The header format is: ";ELC" (4 bytes) followed by version bytes,
    // then comment lines starting with ";" until non-comment content.
    let content = skip_elc_header(&raw_bytes);

    // Save dynamic loader context
    let old_lexical = eval.lexical_binding();
    let old_load_file = eval.obarray().symbol_value("load-file-name").cloned();
    let saved_roots = eval.save_temp_roots();
    if let Some(ref v) = old_load_file {
        eval.push_temp_root(*v);
    }

    // .elc files compiled with lexical-binding will have it in the header comments.
    // Check the raw bytes for the cookie before we stripped the header.
    if elc_has_lexical_binding(&raw_bytes) {
        eval.set_lexical_binding(true);
    }

    eval.set_variable(
        "load-file-name",
        Value::string(path.to_string_lossy().to_string()),
    );

    let result = (|| -> Result<Value, EvalError> {
        // Parse the content as S-expressions using the standard parser.
        // The parser handles #[...], #@N, #$, etc.
        let forms = super::parser::parse_forms(&content).map_err(|e| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "Parse error in {}: {}",
                path.display(),
                e
            ))],
        })?;

        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        for (i, form) in forms.iter().enumerate() {
            tracing::debug!(
                "{} ELC-FORM[{i}/{}]: {}",
                file_name,
                forms.len(),
                print_expr(form).chars().take(100).collect::<String>()
            );

            // Evaluate directly — .elc forms are already compiled, no macro expansion needed.
            let eval_result = eval.eval_expr(form);
            if let Err(ref e) = eval_result {
                let err_detail = match e {
                    EvalError::Signal { symbol, data } => {
                        let sym_name = super::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> =
                            data.iter().map(|v| format_value_for_error(v)).collect();
                        format!("({} {})", sym_name, data_strs.join(" "))
                    }
                    other => format!("{:?}", other),
                };
                tracing::error!(
                    "  !! {file_name} ELC-FORM[{i}] FAILED: {} => {}",
                    print_expr(form).chars().take(120).collect::<String>(),
                    err_detail
                );
            }
            eval_result?;
            eval.gc_safe_point();
        }

        record_load_history(eval, path);
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
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );

    let mut eval = super::eval::Evaluator::new();

    // Set up load-path with lisp/ and its subdirectories.
    let subdirs = [
        "",
        "emacs-lisp",
        "progmodes",
        "language",
        "international",
        "textmodes",
        "vc",
        "leim",
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
        "emacs-lisp/macroexp", // Re-load
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
                    tracing::info!(
                        "  OK: {name} ({:.2?}) [cache hit={dh} miss={dm}]",
                        start.elapsed()
                    );
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
#[path = "load_test.rs"]
mod tests;
