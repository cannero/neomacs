//! File loading and module system (require/provide/load).

use super::builtins::collections::builtin_make_hash_table;
use super::error::{EvalError, Flow, map_flow, signal};
use super::expr::Expr;
use super::expr::print_expr;
use super::intern::{intern, resolve_sym};
use super::keymap::{is_list_keymap, list_keymap_lookup_one};
use super::value::{HashKey, Value, list_to_vec, with_heap, with_heap_mut};
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

fn format_eval_error_in_state(eval: &super::eval::Context, err: &EvalError) -> String {
    match err {
        EvalError::Signal { symbol, data } => {
            let payload = if data.is_empty() {
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

fn is_generated_loaddefs_source(source: &str) -> bool {
    source.contains(GENERATED_LOADDEFS_MARKER)
}

fn eval_generated_form_args(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Vec<Value>, EvalError> {
    args.iter()
        .map(|expr| eval.eval(expr).map_err(map_flow))
        .collect()
}

fn definition_prefixes_table(
    eval: &super::eval::Context,
) -> Result<crate::gc::types::ObjId, EvalError> {
    let value = eval
        .obarray()
        .symbol_value("definition-prefixes")
        .copied()
        .ok_or_else(|| EvalError::Signal {
            symbol: intern("void-variable"),
            data: vec![Value::symbol("definition-prefixes")],
        })?;
    match value {
        Value::HashTable(id) => Ok(id),
        other => Err(EvalError::Signal {
            symbol: intern("wrong-type-argument"),
            data: vec![Value::symbol("hash-table-p"), other],
        }),
    }
}

fn generated_register_definition_prefixes(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("register-definition-prefixes"),
                Value::Int(args.len() as i64),
            ],
        });
    }

    let file = eval.eval(&args[0]).map_err(map_flow)?;
    let prefixes = eval.eval(&args[1]).map_err(map_flow)?;
    let prefixes = list_to_vec(&prefixes).ok_or_else(|| EvalError::Signal {
        symbol: intern("wrong-type-argument"),
        data: vec![Value::symbol("listp"), prefixes],
    })?;
    let table_id = definition_prefixes_table(eval)?;

    let table_test = with_heap(|heap| heap.get_hash_table(table_id).test.clone());
    let keyed_prefixes: Vec<(Value, HashKey)> = prefixes
        .into_iter()
        .map(|prefix| {
            let key = prefix.to_hash_key(&table_test);
            (prefix, key)
        })
        .collect();

    for (prefix, key) in keyed_prefixes {
        let (old, inserting_new_key) = with_heap_mut(|heap| {
            let table = heap.get_hash_table_mut(table_id);
            let old = table.data.get(&key).copied().unwrap_or(Value::Nil);
            let inserting_new_key = !table.data.contains_key(&key);
            (old, inserting_new_key)
        });

        let new = Value::cons(file, old);

        with_heap_mut(|heap| {
            let table = heap.get_hash_table_mut(table_id);
            table.data.insert(key.clone(), new);
            if inserting_new_key {
                table.key_snapshots.insert(key.clone(), prefix);
                table.insertion_order.push(key);
            }
        });
    }

    Ok(Value::Nil)
}

fn generated_custom_autoload(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if !(2..=3).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("custom-autoload"),
                Value::Int(args.len() as i64),
            ],
        });
    }

    let values = eval_generated_form_args(eval, args)?;
    let symbol = values[0];
    let load = values[1];
    let noset = values.get(2).copied().unwrap_or(Value::Nil);
    let custom_autoload = if noset.is_nil() {
        Value::True
    } else {
        Value::symbol("noset")
    };

    super::builtins::builtin_put(
        eval,
        vec![symbol, Value::symbol("custom-autoload"), custom_autoload],
    )
    .map_err(map_flow)?;

    let current_loads =
        super::builtins::builtin_get(eval, vec![symbol, Value::symbol("custom-loads")])
            .map_err(map_flow)?;
    let present = !super::builtins::builtin_member(vec![load, current_loads])
        .map_err(map_flow)?
        .is_nil();
    if !present {
        super::builtins::builtin_put(
            eval,
            vec![
                symbol,
                Value::symbol("custom-loads"),
                Value::cons(load, current_loads),
            ],
        )
        .map_err(map_flow)?;
    }

    Ok(Value::Nil)
}

fn generated_function_put(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![Value::symbol("function-put"), Value::Int(args.len() as i64)],
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    super::builtins::builtin_put(eval, values).map_err(map_flow)
}

fn constant_obsolete_error(name: Value) -> EvalError {
    EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "Can't make `{}` obsolete; did you forget a quote mark?",
            format_value_for_error(&name)
                .trim_matches('"')
                .trim_matches('(')
                .trim_matches(')')
        ))],
    }
}

fn generated_make_obsolete(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("make-obsolete"),
                Value::Int(args.len() as i64),
            ],
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    let obsolete_name = values[0];
    if matches!(obsolete_name, Value::Nil | Value::True) {
        return Err(constant_obsolete_error(obsolete_name));
    }
    let info = Value::list(vec![values[1], Value::Nil, values[2]]);
    super::builtins::builtin_put(
        eval,
        vec![obsolete_name, Value::symbol("byte-obsolete-info"), info],
    )
    .map_err(map_flow)?;
    Ok(obsolete_name)
}

fn generated_make_obsolete_variable(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if !(3..=4).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("make-obsolete-variable"),
                Value::Int(args.len() as i64),
            ],
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    let obsolete_name = values[0];
    if matches!(obsolete_name, Value::Nil | Value::True) {
        return Err(constant_obsolete_error(obsolete_name));
    }
    let access_type = values.get(3).copied().unwrap_or(Value::Nil);
    let info = Value::list(vec![values[1], access_type, values[2]]);
    super::builtins::builtin_put(
        eval,
        vec![obsolete_name, Value::symbol("byte-obsolete-variable"), info],
    )
    .map_err(map_flow)?;
    Ok(obsolete_name)
}

fn generated_defalias(eval: &mut super::eval::Context, args: &[Expr]) -> Result<Value, EvalError> {
    if !(2..=3).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![Value::symbol("defalias"), Value::Int(args.len() as i64)],
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

fn copy_symbol_property_if_absent(
    eval: &mut super::eval::Context,
    from_symbol: Value,
    to_symbol: Value,
    property: &str,
) -> Result<(), EvalError> {
    let current = super::builtins::builtin_get(eval, vec![to_symbol, Value::symbol(property)])
        .map_err(map_flow)?;
    if !current.is_nil() {
        return Ok(());
    }
    let source = super::builtins::builtin_get(eval, vec![from_symbol, Value::symbol(property)])
        .map_err(map_flow)?;
    if source.is_nil() {
        return Ok(());
    }
    super::builtins::builtin_put(eval, vec![to_symbol, Value::symbol(property), source])
        .map_err(map_flow)?;
    Ok(())
}

fn generated_define_obsolete_function_alias(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if !(3..=4).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("define-obsolete-function-alias"),
                Value::Int(args.len() as i64),
            ],
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    let result = eval
        .defalias_value(values[0], values[1])
        .map_err(map_flow)?;
    if let Some(doc) = values.get(3).copied().filter(|value| !value.is_nil()) {
        super::builtins::builtin_put(
            eval,
            vec![values[0], Value::symbol("function-documentation"), doc],
        )
        .map_err(map_flow)?;
    }
    let info = Value::list(vec![values[1], Value::Nil, values[2]]);
    super::builtins::builtin_put(
        eval,
        vec![values[0], Value::symbol("byte-obsolete-info"), info],
    )
    .map_err(map_flow)?;
    Ok(result)
}

fn generated_define_obsolete_variable_alias(
    eval: &mut super::eval::Context,
    args: &[Expr],
) -> Result<Value, EvalError> {
    if !(3..=4).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![
                Value::symbol("define-obsolete-variable-alias"),
                Value::Int(args.len() as i64),
            ],
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    let result = super::builtins::builtin_defvaralias(
        eval,
        vec![
            values[0],
            values[1],
            values.get(3).copied().unwrap_or(Value::Nil),
        ],
    )
    .map_err(map_flow)?;
    copy_symbol_property_if_absent(eval, values[0], values[1], "saved-value")?;
    copy_symbol_property_if_absent(eval, values[0], values[1], "saved-variable-comment")?;
    let info = Value::list(vec![values[1], Value::Nil, values[2]]);
    super::builtins::builtin_put(
        eval,
        vec![values[0], Value::symbol("byte-obsolete-variable"), info],
    )
    .map_err(map_flow)?;
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
    match resolve_sym(*id) {
        "progn" => {
            let mut last = Value::Nil;
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
        "register-definition-prefixes" => {
            Ok(Some(generated_register_definition_prefixes(eval, tail)?))
        }
        "custom-autoload" => Ok(Some(generated_custom_autoload(eval, tail)?)),
        "function-put" => Ok(Some(generated_function_put(eval, tail)?)),
        "put" => {
            let values = eval_generated_form_args(eval, tail)?;
            Ok(Some(
                super::builtins::builtin_put(eval, values).map_err(map_flow)?,
            ))
        }
        "make-obsolete" => Ok(Some(generated_make_obsolete(eval, tail)?)),
        "make-obsolete-variable" => Ok(Some(generated_make_obsolete_variable(eval, tail)?)),
        "defalias" => Ok(Some(generated_defalias(eval, tail)?)),
        "define-obsolete-function-alias" => {
            Ok(Some(generated_define_obsolete_function_alias(eval, tail)?))
        }
        "define-obsolete-variable-alias" => {
            Ok(Some(generated_define_obsolete_variable_alias(eval, tail)?))
        }
        "defvaralias" => {
            let values = eval_generated_form_args(eval, tail)?;
            Ok(Some(
                super::builtins::builtin_defvaralias(eval, values).map_err(map_flow)?,
            ))
        }
        "defvar-local" => Ok(Some(
            super::custom::sf_defvar_local(eval, tail).map_err(map_flow)?,
        )),
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
    eval.eval_expr(form)
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
    let file = match file {
        Value::Str(id) => with_heap(|h| h.get_string(id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), other],
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
                Ok(LoadPlan::Return(Value::Nil))
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
            vec![Value::symbol("load"), Value::Int(0)],
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
                    EvalError::Signal { symbol, data } => {
                        crate::emacs_core::error::signal(resolve_sym(symbol), data)
                    }
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

pub(crate) fn lexical_binding_enabled_for_source(source: &str) -> bool {
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

/// Check if eager macro expansion is available.
/// Requires both `internal-macroexpand-for-load` and the pcase backquote
/// macroexpander (`--pcase-macroexpander`) to be defined, since
/// `macroexpand-all` uses pcase backquote patterns internally.
#[tracing::instrument(level = "debug", skip(eval))]
pub(crate) fn get_eager_macroexpand_fn(eval: &super::eval::Context) -> Option<Value> {
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
pub(crate) fn eager_expand_eval(
    eval: &mut super::eval::Context,
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

    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(old_lexenv);
    if let Some(ref v) = old_load_file {
        eval.push_temp_root(*v);
    }

    if lexical_binding {
        eval.set_lexical_binding(true);
        eval.lexenv = Value::list(vec![Value::True]);
    }

    eval.set_variable(
        "load-file-name",
        Value::string(path.to_string_lossy().to_string()),
    );

    let result = body(eval);

    eval.set_lexical_binding(old_lexical);
    eval.lexenv = old_lexenv;
    if let Some(old) = old_load_file {
        eval.set_variable("load-file-name", old);
    } else {
        eval.set_variable("load-file-name", Value::Nil);
    }
    eval.restore_temp_roots(saved_roots);

    result
}

/// Shared form-by-form evaluation loop, modelled after GNU Emacs `readevalloop`
/// in lread.c.
///
/// Iterates over `forms`, logging each form and its timing, reporting errors
/// with human-readable detail, and calling `gc_safe_point` between forms.
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
        });
    }

    // GNU Emacs allows three nested loads of the same file and signals an
    // error on the fourth.  Matching that behavior matters because Lisp
    // sometimes depends on the signal shape rather than on silent skipping.
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
        });
    }
    eval.loads_in_progress.push(canonical);

    // GNU Emacs lread.c: specbind(Qload_in_progress, Qt)
    // Set load-in-progress to t during file loading, restore afterward.
    let old_load_in_progress = eval
        .obarray()
        .symbol_value("load-in-progress")
        .cloned()
        .unwrap_or(Value::Nil);
    eval.set_variable("load-in-progress", Value::True);

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
                    Value::bool(noerror),
                    Value::bool(nomessage),
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
        lexical_binding_enabled_for_source(&content)
    };

    // --- .el-only: check for pre-compiled .neobc cache before setting up context ---
    if !is_elc {
        let neobc_path = path.with_extension("neobc");
        if neobc_path.exists() {
            let source_hash = super::file_compile_format::source_sha256(&content);
            if let Ok(loaded) = super::file_compile_format::read_neobc(&neobc_path, &source_hash) {
                tracing::info!(
                    "neobc cache hit for {} ({} forms)",
                    path.display(),
                    loaded.forms.len()
                );
                return with_load_context(eval, path, loaded.lexical_binding, |eval| {
                    for form in &loaded.forms {
                        match form {
                            super::file_compile_format::LoadedForm::Eval(expr) => {
                                eval.eval_expr(expr)?;
                            }
                            super::file_compile_format::LoadedForm::Constant(_) => {
                                // eval-when-compile constant -- already evaluated, skip.
                            }
                        }
                        eval.gc_safe_point();
                    }
                    record_load_history(eval, path);
                    Ok(Value::True)
                });
            }
        }
    }

    // --- Shared context setup via with_load_context ---
    with_load_context(eval, path, lexical_binding, |eval| {
        // .el-only: generated loaddefs fast path
        if !is_elc {
            // Clear pointer-identity caches before each source file.
            eval.macro_expansion_cache.clear();
            eval.literal_cache.clear();

            let generated_loaddefs = is_generated_loaddefs_source(&content);
            if generated_loaddefs {
                let forms = parse_source_forms(path, &content)?;
                tracing::info!(
                    "generated loaddefs replay for {} ({} forms)",
                    path.display(),
                    forms.len()
                );
                for form in &forms {
                    eval_generated_loaddefs_form(eval, form)?;
                    eval.gc_safe_point();
                }
                record_load_history(eval, path);
                return Ok(Value::True);
            }
        }

        // Eager macro expansion guard (.el only -- .elc has macros compiled away).
        let macroexpand_fn: Option<Value> = if !is_elc {
            get_eager_macroexpand_fn(eval)
        } else {
            None
        };

        // --- Parse forms ---
        let forms = if is_elc {
            super::parser::parse_forms(&content).map_err(|e| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "Parse error in {}: {}",
                    path.display(),
                    e
                ))],
            })?
        } else {
            parse_source_forms(path, &content)?
        };

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
                eval.eval_expr(&reified)
            })?;
            record_load_history(eval, path);
            return Ok(Value::True);
        }

        // --- .el path: parse forms, macroexpand+eval each form, record history ---
        if let Some(mexp_fn) = macroexpand_fn {
            readevalloop(eval, &file_name, &forms, |eval, _i, form| {
                let form_value = eval.quote_to_runtime_value(form);
                eager_expand_eval(eval, form_value, mexp_fn)
            })?;
        } else {
            readevalloop(eval, &file_name, &forms, |eval, _i, form| {
                eval.eval_expr(form)
            })?;
        }

        record_load_history(eval, path);

        // Emacs `load` returns non-nil on success (typically `t`).
        Ok(Value::True)
    })
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
    let entry = Value::cons(Value::string(path_str.clone()), Value::Nil);
    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::Nil);
    let filtered_history = Value::list(
        list_to_vec(&history)
            .unwrap_or_default()
            .into_iter()
            .filter(|existing| match existing {
                Value::Cons(id) => with_heap(|heap| {
                    heap.cons_car(*id)
                        .as_str()
                        .is_none_or(|loaded| loaded != path_str)
                }),
                _ => true,
            })
            .collect(),
    );
    eval.set_variable("load-history", Value::cons(entry, filtered_history));

    // GNU Emacs lread.c:1540-1541: after loading a file, call
    // (do-after-load-evaluation FILENAME) to run eval-after-load hooks.
    let dale_id = super::intern::intern("do-after-load-evaluation");
    let is_fboundp = eval
        .obarray()
        .symbol_function_id(dale_id)
        .is_some_and(|f| !f.is_nil());
    if is_fboundp {
        let abs_path = Value::string(path_str.clone());
        if let Err(e) = eval.apply(Value::Symbol(dale_id), vec![abs_path]) {
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
}

/// Register bootstrap variables owned by the file-loading subsystem.
pub fn register_bootstrap_vars(obarray: &mut super::symbol::Obarray) {
    obarray.set_symbol_value("after-load-alist", Value::Nil);
    obarray.set_symbol_value("macroexp--dynvars", Value::Nil);
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
// represent correctly. V15 invalidates older caches because GUI startup now
// boots through term/neo-win with window-system `neo`; older dumps preserve
// the previous backend identity surface.
const BOOTSTRAP_IMAGE_SCHEMA_VERSION: u32 = 15;
const BOOTSTRAP_CACHE_SEED: &str = match option_env!("NEOVM_BOOTSTRAP_CACHE_SEED") {
    Some(seed) => seed,
    None => "dev",
};
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

fn bootstrap_dump_path(runtime_root: &Path, extra_features: &[&str]) -> PathBuf {
    let features = normalized_bootstrap_features(extra_features);
    let suffix = if features.is_empty() {
        String::new()
    } else {
        format!("-{}", features.join("-"))
    };
    bootstrap_cache_dir(runtime_root).join(format!(
        "neovm-bootstrap-v{BOOTSTRAP_IMAGE_SCHEMA_VERSION}-{BOOTSTRAP_CACHE_SEED}{suffix}.pdump"
    ))
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
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "bootstrap cache lock: failed locking {}: {}",
                    lock_path.display(),
                    std::io::Error::last_os_error()
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
        ("exec-directory", Value::Nil),
        ("configure-info-directory", Value::Nil),
        ("charset-map-path", Value::Nil),
        ("initial-environment", process_environment.clone()),
        ("process-environment", process_environment),
        ("path-separator", Value::string(path_separator)),
        ("file-name-coding-system", Value::Nil),
        ("default-file-name-coding-system", Value::Nil),
        ("set-auto-coding-function", Value::Nil),
        ("after-insert-file-functions", Value::Nil),
        ("write-region-annotate-functions", Value::Nil),
        ("write-region-post-annotation-function", Value::Nil),
        ("write-region-annotations-so-far", Value::Nil),
        ("inhibit-file-name-handlers", Value::Nil),
        ("inhibit-file-name-operation", Value::Nil),
        (
            "temporary-file-directory",
            Value::string(temporary_file_directory),
        ),
        ("create-lockfiles", Value::True),
        ("auto-save-list-file-name", Value::Nil),
        ("auto-save-list-file-prefix", Value::Nil),
        ("auto-save-visited-file-name", Value::Nil),
        ("auto-save-include-big-deletions", Value::Nil),
        ("shared-game-score-directory", Value::Nil),
        ("invocation-name", Value::Nil),
        ("invocation-directory", Value::Nil),
        ("system-messages-locale", Value::Nil),
        ("system-time-locale", Value::Nil),
        ("before-init-time", Value::Nil),
        ("after-init-time", Value::Nil),
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
        ("delayed-warnings-list", Value::Nil),
        ("default-text-properties", Value::Nil),
        ("char-property-alias-alist", Value::Nil),
        ("inhibit-point-motion-hooks", Value::True),
        (
            "text-property-default-nonsticky",
            Value::list(vec![
                Value::cons(Value::symbol("syntax-table"), Value::True),
                Value::cons(Value::symbol("display"), Value::True),
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
        Expr::Int(v) => Some(Value::Int(*v)),
        Expr::Symbol(id) => match resolve_sym(*id) {
            "nil" => Some(Value::Nil),
            "t" => Some(Value::True),
            name => Some(Value::symbol(name)),
        },
        Expr::Keyword(id) => Some(Value::symbol(resolve_sym(*id))),
        Expr::Str(s) => Some(Value::string(s.clone())),
        Expr::Char(c) => Some(Value::Char(*c)),
        Expr::List(_) => expr_quoted_symbol_name(expr).map(|name| Value::symbol(&name)),
        _ => None,
    }
}

fn hidden_cl_runtime_entry_points() -> std::collections::BTreeSet<String> {
    // GNU Emacs -Q does not expose these cl-loaddefs entry points until
    // cl-lib/eieio explicitly restore them via the real Lisp load path.
    // Source bootstrap currently leaks just this small surface via
    // eval-when-compile; hide only the proven leaked names here instead of
    // stripping whole cl-* files out of the runtime image.
    //
    // NOTE: cl--block-wrapper and cl--block-throw are intentionally kept
    // available. NeoVM loads .el source (not .elc) and runs macroexpand-all
    // during eager expansion, which triggers compiler-macros that require
    // these functions (e.g. cl--block-wrapper is aliased to #'identity and
    // its compiler-macro optimizes away unused cl-block wrappers).
    [
        "cl-every",
        "cl-defstruct",
        "cl-reduce",
        "cl-subseq",
        "gv-get",
        "setf",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn collect_runtime_loaddefs_autoload_args(
    expr: &Expr,
    restore_autoload_files: &[&str],
    restore_names: &mut std::collections::BTreeSet<String>,
    out: &mut Vec<Vec<Value>>,
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
    if !restore_autoload_files.contains(&file.as_str()) {
        return;
    }

    restore_names.insert(name.clone());
    let mut args = vec![Value::symbol(&name), Value::string(file.clone())];
    for expr in items.iter().skip(3).take(3) {
        let Some(value) = expr_runtime_value(expr) else {
            return;
        };
        args.push(value);
    }
    out.push(args);
}

fn collect_runtime_loaddefs_property_forms(
    expr: &Expr,
    restore_names: &std::collections::BTreeSet<String>,
    out: &mut Vec<Expr>,
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
    if restore_names.contains(&name) {
        out.push(expr.clone());
    }
}

fn runtime_loaddefs_restore_state(
    project_root: &Path,
    restore_autoload_files: &[&str],
) -> Result<(Vec<Vec<Value>>, Vec<Expr>), EvalError> {
    // GNU Emacs -Q runtime carries only the core loaddefs surface from
    // ldefs-boot.el here (not cl-loaddefs.el).  The Common Lisp entry points
    // such as cl-every/cl-reduce/cl-defstruct become visible only after
    // `cl-lib.el` is required, because cl-lib.el itself loads cl-loaddefs.
    let loaddefs_paths = [project_root.join("lisp/ldefs-boot.el")];

    let mut args = Vec::new();
    let mut restore_names = std::collections::BTreeSet::new();
    let mut property_forms = Vec::new();

    for loaddefs_path in loaddefs_paths {
        let source = fs::read_to_string(&loaddefs_path).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "bootstrap runtime cleanup: failed reading {}: {err}",
                loaddefs_path.display()
            ))],
        })?;
        let forms =
            crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "bootstrap runtime cleanup: failed parsing {}: {err}",
                    loaddefs_path.display()
                ))],
            })?;

        for form in &forms {
            collect_runtime_loaddefs_autoload_args(
                form,
                restore_autoload_files,
                &mut restore_names,
                &mut args,
            );
        }
        for form in &forms {
            collect_runtime_loaddefs_property_forms(form, &restore_names, &mut property_forms);
        }
    }
    Ok((args, property_forms))
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
    })?;
    let forms =
        crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "ldefs-boot autoload restore: failed parsing {}: {err}",
                ldefs_path.display()
            ))],
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
    // GNU -Q does not leave these features present in the default runtime
    // surface even though source bootstrap can transiently load them.
    let compile_only_features = ["cl-lib", "cl-macs", "cl-seq", "cl-extra", "gv"];
    // GNU keeps gv entry points available via ldefs-boot autoloads even after
    // the gv feature itself is no longer present at -Q runtime startup.
    let runtime_autoload_files = ["gv", "icons"];
    let (restore_autoload_args, restore_property_forms) =
        runtime_loaddefs_restore_state(project_root, &runtime_autoload_files)?;
    let mut compile_only_names = hidden_cl_runtime_entry_points();
    // The current dumped nadvice bytecode still dereferences gv refs via this
    // runtime helper. Stripping it here leaves the cached bootstrap image
    // internally inconsistent even though `featurep 'gv` remains nil.
    compile_only_names.remove("gv-deref");

    for feature in compile_only_features {
        eval.remove_feature(feature);
    }
    strip_runtime_icons_surface(eval);

    for name in &compile_only_names {
        eval.obarray_mut().fmakunbound(&name);
        eval.autoloads.remove(name);
        let _ = super::builtins::builtin_put(
            eval,
            vec![
                Value::symbol(name),
                Value::symbol("autoload-macro"),
                Value::Nil,
            ],
        );
    }

    let autoload_entries = eval.autoloads.entries_snapshot();
    for entry in &autoload_entries {
        if runtime_autoload_files.contains(&entry.file.as_str())
            || compile_only_names.contains(&entry.name)
        {
            eval.autoloads.remove(&entry.name);
            let _ = super::builtins::builtin_put(
                eval,
                vec![
                    Value::symbol(&entry.name),
                    Value::symbol("autoload-macro"),
                    Value::Nil,
                ],
            );
        }
    }

    for args in restore_autoload_args {
        super::autoload::builtin_autoload(eval, args).map_err(map_flow)?;
    }
    for form in &restore_property_forms {
        eval.eval_expr(form)?;
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
                Value::Nil,
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
    })?;
    let forms = crate::emacs_core::parser::parse_forms(&source[start..]).map_err(|err| {
        EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "runtime prefix restore: failed parsing {} from {marker}: {err:?}",
                path.display()
            ))],
        }
    })?;
    let form = forms.first().ok_or_else(|| EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "runtime prefix restore: no GNU subr form after {marker}"
        ))],
    })?;
    eval.eval_expr(form)?;
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

    let esc = list_keymap_lookup_one(&global, &Value::Int(27));
    let ctl_x = list_keymap_lookup_one(&global, &Value::Int(24));
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
        })?;
        let forms =
            crate::emacs_core::parser::parse_forms(&source).map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "runtime global prefix repair: failed parsing {}: {err:?}",
                    path.display()
                ))],
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
            eval.eval_expr(form)?;
        }
    }

    Ok(())
}

fn finalize_cached_bootstrap_eval(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    // Register all builtins — pdump doesn't preserve the subr_registry
    // or obarray function cells for builtins registered via defsubr.
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
    eval_startup_forms(
        eval,
        r#"
          ;; Load icons.el early — many packages (tab-bar, modeline, doom)
          ;; call icon-string during display. GNU loads it lazily via
          ;; (require 'icons) inside function bodies, but NeoVM's display
          ;; path may call icon-string before the lazy require triggers.
          (require 'icons nil t)
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

    let filter_fn = eval
        .obarray()
        .symbol_value("internal-make-interpreted-closure-function")
        .cloned()
        .and_then(|value| match value {
            Value::Symbol(sym) if resolve_sym(sym) == "cconv-make-interpreted-closure" => eval
                .obarray()
                .symbol_function("cconv-make-interpreted-closure")
                .cloned(),
            _ => None,
        });
    eval.set_interpreted_closure_filter_fn(filter_fn);

    Ok(())
}

fn install_bootstrap_x_window_system_vars(
    eval: &mut super::eval::Context,
) -> Result<(), EvalError> {
    let keysym_table = builtin_make_hash_table(vec![
        Value::keyword(":test"),
        Value::symbol("eql"),
        Value::keyword(":size"),
        Value::Int(900),
    ])
    .map_err(map_flow)?;
    eval.set_variable("x-keysym-table", keysym_table);
    eval.set_variable("x-selection-timeout", Value::Int(0));
    eval.set_variable("x-session-id", Value::Nil);
    eval.set_variable("x-session-previous-id", Value::Nil);
    for name in [
        "x-ctrl-keysym",
        "x-alt-keysym",
        "x-hyper-keysym",
        "x-meta-keysym",
        "x-super-keysym",
    ] {
        eval.set_variable(name, Value::Nil);
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

pub fn create_bootstrap_evaluator_with_features(
    extra_features: &[&str],
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
        // loadup.el line 60: (null dump-mode) triggers load-path setup.
        // Setting to nil skips the dump section (line 604) entirely —
        // NeoVM handles pdump in Rust after loadup.el returns.
        eval.set_variable("dump-mode", Value::Nil);
        eval.set_variable("purify-flag", Value::Nil);
        // NeoVM counts depth more aggressively than GNU (see eval.rs comment).
        eval.set_variable("max-lisp-eval-depth", Value::Int(2400));
        eval.set_variable("inhibit-load-charset-map", Value::True);
        // Override Elisp function-get with Rust builtin to avoid deep
        // eval depth consumption. The Elisp version from subr.el uses
        // get/fboundp/symbol-function which each increment depth in NeoVM
        // (but not in GNU's C implementations).
        eval.obarray
            .set_symbol_function("function-get", Value::Subr(intern("function-get")));
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
/// The dump file is automatically invalidated when the pdump format
/// version changes. Set `NEOVM_DISABLE_PDUMP=1` to force fresh bootstrap.
pub fn create_bootstrap_evaluator_cached() -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_cached_with_features(&[])
}

pub fn create_bootstrap_evaluator_cached_with_features(
    extra_features: &[&str],
) -> Result<super::eval::Context, EvalError> {
    let project_root = runtime_project_root();
    let dump_path = bootstrap_dump_path(&project_root, extra_features);
    create_bootstrap_evaluator_cached_at_path(extra_features, &dump_path)
}

fn create_bootstrap_evaluator_cached_at_path(
    extra_features: &[&str],
    dump_path: &Path,
) -> Result<super::eval::Context, EvalError> {
    use super::pdump;

    let project_root = runtime_project_root();

    // Allow disabling pdump via env var
    if std::env::var("NEOVM_DISABLE_PDUMP").unwrap_or_default() == "1" {
        let mut eval = create_bootstrap_evaluator_with_features(extra_features)?;
        finalize_cached_bootstrap_eval(&mut eval, &project_root)?;
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
                finalize_cached_bootstrap_eval(&mut eval, &project_root)?;

                return Ok(eval);
            }
            Err(e) => {
                tracing::warn!("pdump: load failed ({e}), falling back to full bootstrap");
            }
        }
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
                finalize_cached_bootstrap_eval(&mut eval, &project_root)?;
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
                    finalize_cached_bootstrap_eval(&mut loaded, &project_root)?;
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
