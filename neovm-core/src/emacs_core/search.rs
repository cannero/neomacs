//! Search and regex builtins for the Elisp interpreter.
//!
//! Pure builtins:
//! - `string-match`, `string-match-p`, `regexp-quote`
//! - `match-beginning`, `match-end`, `match-data`, `set-match-data`
//! - `looking-at`, `looking-at-p`, `replace-regexp-in-string`
//!
//! Eval-dependent builtins:
//! - `search-forward`, `search-backward`
//! - `re-search-forward`, `re-search-backward`
//! - `posix-search-forward`, `posix-search-backward`
//! - `replace-match`
//! - `word-search-forward`, `word-search-backward`

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::*;
use crate::emacs_core::value::ValueKind;

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

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
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

fn expect_integer_or_marker(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )),
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val
            .as_runtime_string_owned()
            .expect("ValueKind::String must carry LispString payload")),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_lisp_string(val: &Value) -> Result<&'static crate::heap_types::LispString, Flow> {
    val.as_lisp_string()
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), *val]))
}

fn cloned_lisp_string_value(string: &crate::heap_types::LispString) -> Value {
    Value::heap_string(string.clone())
}

fn regexp_quote_lisp_string(
    input: &crate::heap_types::LispString,
) -> crate::heap_types::LispString {
    let mut out = Vec::with_capacity(input.as_bytes().len() + 8);
    for &byte in input.as_bytes() {
        match byte {
            b'.' | b'*' | b'+' | b'?' | b'[' | b'^' | b'$' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            _ => out.push(byte),
        }
    }

    if input.is_multibyte() {
        crate::heap_types::LispString::from_emacs_bytes(out)
    } else {
        crate::heap_types::LispString::from_unibyte(out)
    }
}

fn normalize_string_start_arg(string: &str, start: Option<&Value>) -> Result<usize, Flow> {
    let Some(start_val) = start else {
        return Ok(0);
    };
    if start_val.is_nil() {
        return Ok(0);
    }

    let raw_start = expect_int(start_val)?;
    let string_bytes = string.as_bytes();
    let len = crate::emacs_core::emacs_char::chars_in_multibyte(string_bytes) as i64;
    let normalized = if raw_start < 0 {
        len.checked_add(raw_start)
    } else {
        Some(raw_start)
    };

    let Some(start_idx) = normalized else {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::fixnum(raw_start)],
        ));
    };

    if !(0..=len).contains(&start_idx) {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::fixnum(raw_start)],
        ));
    }

    let start_char_idx = start_idx as usize;
    if start_char_idx == len as usize {
        return Ok(string.len());
    }

    Ok(crate::emacs_core::emacs_char::char_to_byte_pos(
        string_bytes,
        start_char_idx,
    ))
}

pub(crate) fn normalize_lisp_string_start_arg(
    string: &crate::heap_types::LispString,
    start: Option<&Value>,
) -> Result<usize, Flow> {
    let Some(start_val) = start else {
        return Ok(0);
    };
    if start_val.is_nil() {
        return Ok(0);
    }

    let raw_start = expect_int(start_val)?;
    if !string.is_multibyte() {
        let len = string.byte_len() as i64;
        let normalized = if raw_start < 0 {
            len.checked_add(raw_start)
        } else {
            Some(raw_start)
        };
        let Some(start_idx) = normalized else {
            return Err(signal(
                "args-out-of-range",
                vec![cloned_lisp_string_value(string), Value::fixnum(raw_start)],
            ));
        };
        if !(0..=len).contains(&start_idx) {
            return Err(signal(
                "args-out-of-range",
                vec![cloned_lisp_string_value(string), Value::fixnum(raw_start)],
            ));
        }
        return Ok(start_idx as usize);
    }

    let len = string.schars() as i64;
    let normalized = if raw_start < 0 {
        len.checked_add(raw_start)
    } else {
        Some(raw_start)
    };
    let Some(start_idx) = normalized else {
        return Err(signal(
            "args-out-of-range",
            vec![cloned_lisp_string_value(string), Value::fixnum(raw_start)],
        ));
    };
    if !(0..=len).contains(&start_idx) {
        return Err(signal(
            "args-out-of-range",
            vec![cloned_lisp_string_value(string), Value::fixnum(raw_start)],
        ));
    }
    let start_char_idx = start_idx as usize;
    if start_char_idx == len as usize {
        return Ok(string.byte_len());
    }
    Ok(crate::emacs_core::emacs_char::char_to_byte_pos(
        string.as_bytes(),
        start_char_idx,
    ))
}

fn flatten_match_data(md: &super::regex::MatchData) -> Value {
    let mut trailing = md.groups.len();
    while trailing > 0 && md.groups[trailing - 1].is_none() {
        trailing -= 1;
    }

    let mut flat: Vec<Value> = Vec::with_capacity(trailing * 2);
    for grp in md.groups.iter().take(trailing) {
        match grp {
            Some((start, end)) => {
                // For string searches, positions are already character positions.
                // For buffer searches, positions are byte positions (returned as-is).
                flat.push(Value::fixnum(*start as i64));
                flat.push(Value::fixnum(*end as i64));
            }
            None => {
                flat.push(Value::NIL);
                flat.push(Value::NIL);
            }
        }
    }
    Value::list(flat)
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(regexp-quote STRING)` -- return a regexp that matches STRING literally,
/// quoting all special regex characters.
pub(crate) fn builtin_regexp_quote(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::perf_trace::time_op(
        crate::emacs_core::perf_trace::HotpathOp::RegexpQuote,
        || {
            expect_args("regexp-quote", &args, 1)?;
            let string = expect_lisp_string(&args[0])?;
            Ok(Value::heap_string(regexp_quote_lisp_string(string)))
        },
    )
}

fn parse_replace_regexp_subexp_start_lisp(
    args: &[Value],
    string: &crate::heap_types::LispString,
) -> Result<(usize, usize), Flow> {
    let subexp = match args.get(5) {
        Some(v) if v.is_nil() => 0i64,
        None => 0i64,
        Some(value) => expect_int(value)?,
    };
    if subexp < 0 {
        return Err(signal(
            "args-out-of-range",
            vec![
                Value::fixnum(subexp),
                Value::fixnum(0),
                Value::fixnum(string.schars() as i64),
            ],
        ));
    }
    let start = normalize_lisp_string_start_arg(string, args.get(6))?;
    Ok((subexp as usize, start))
}

fn storage_string_from_lisp_string(string: &crate::heap_types::LispString) -> String {
    crate::emacs_core::string_escape::emacs_bytes_to_storage_string(
        string.as_bytes(),
        string.is_multibyte(),
    )
}

fn storage_string_to_lisp_string(text: &str, multibyte: bool) -> crate::heap_types::LispString {
    let bytes = crate::emacs_core::string_escape::storage_string_to_buffer_bytes(text, multibyte);
    if multibyte {
        crate::heap_types::LispString::from_emacs_bytes(bytes)
    } else {
        crate::heap_types::LispString::from_unibyte(bytes)
    }
}

fn translate_match_data_to_substring(
    match_data: &super::regex::MatchData,
    delta: i64,
    searched_string: super::regex::SearchedString,
) -> super::regex::MatchData {
    let mut translated = match_data.clone();
    for group in translated.groups.iter_mut() {
        if let Some((start, end)) = group {
            *start = (*start as i64 + delta).max(0) as usize;
            *end = (*end as i64 + delta).max(0) as usize;
        }
    }
    translated.searched_string = Some(searched_string);
    translated.searched_buffer = None;
    translated.buffer_positions_are_bytes = false;
    translated
}

fn replace_match_on_substring(
    source: &crate::heap_types::LispString,
    replacement: &crate::heap_types::LispString,
    fixedcase: bool,
    literal: bool,
    subexp: usize,
    match_data: &Option<super::regex::MatchData>,
) -> Result<crate::heap_types::LispString, Flow> {
    let source_storage = storage_string_from_lisp_string(source);
    let replacement_storage = storage_string_from_lisp_string(replacement);
    let result = super::regex::replace_match_string_with_syntax(
        &source_storage,
        &replacement_storage,
        fixedcase,
        literal,
        subexp,
        match_data,
        None,
        false,
    )
    .map_err(|msg| signal("error", vec![Value::string(msg)]))?;
    Ok(storage_string_to_lisp_string(
        &result,
        source.is_multibyte() || replacement.is_multibyte(),
    ))
}

fn concat_lisp_string_pieces(
    pieces: Vec<crate::heap_types::LispString>,
) -> crate::heap_types::LispString {
    let mut iter = pieces.into_iter();
    let Some(mut acc) = iter.next() else {
        return crate::heap_types::LispString::from_unibyte(Vec::new());
    };
    for piece in iter {
        acc = acc.concat(&piece);
    }
    acc
}

fn replace_regexp_in_string_lisp<F>(
    args: &[Value],
    case_fold: bool,
    mut replacement_for_match: F,
) -> EvalResult
where
    F: FnMut(
        &crate::heap_types::LispString,
        &Option<super::regex::MatchData>,
    ) -> Result<crate::heap_types::LispString, Flow>,
{
    let pattern = expect_lisp_string(&args[0])?;
    let source = expect_lisp_string(&args[2])?;
    let (_, start) = parse_replace_regexp_subexp_start_lisp(args, source)?;
    let mut cursor = start;
    let mut search_at = start;
    let mut pieces = Vec::new();
    let mut match_data = None;
    let total_chars = source.schars();

    // GNU `replace-regexp-in-string` searches the original Lisp string,
    // translates match data onto the matched substring, then runs
    // `replace-match` semantics on that substring.
    while search_at < source.byte_len() {
        let found = super::regex::string_match_full_with_case_fold_source_lisp_pattern_posix(
            pattern,
            source,
            super::regex::SearchedString::Heap(args[2]),
            search_at,
            case_fold,
            false,
            &mut match_data,
        )
        .map_err(|msg| signal("invalid-regexp", vec![Value::string(msg)]))?;
        if found.is_none() {
            break;
        }

        let Some(current_md) = match_data.clone() else {
            break;
        };
        let Some((full_start_char, full_end_char)) = current_md.groups.first().and_then(|g| *g)
        else {
            break;
        };

        let match_span_end_char = if full_start_char == full_end_char {
            (full_start_char + 1).min(total_chars)
        } else {
            full_end_char
        };
        let full_start_byte = super::regex::char_pos_to_byte_lisp_string(source, full_start_char);
        let match_span_end_byte =
            super::regex::char_pos_to_byte_lisp_string(source, match_span_end_char);

        pieces.push(
            source
                .slice(cursor, full_start_byte)
                .expect("validated match prefix must slice"),
        );

        let match_span = source
            .slice(full_start_byte, match_span_end_byte)
            .expect("validated match span must slice");
        let translated_md = Some(translate_match_data_to_substring(
            &current_md,
            -(full_start_char as i64),
            super::regex::SearchedString::Owned(match_span.clone()),
        ));
        pieces.push(replacement_for_match(&match_span, &translated_md)?);
        cursor = match_span_end_byte;
        search_at = match_span_end_byte;
    }

    pieces.push(
        source
            .slice(cursor, source.byte_len())
            .expect("validated match tail must slice"),
    );
    Ok(Value::heap_string(concat_lisp_string_pieces(pieces)))
}

/// Route symbol-value reads through the full GNU lookup path so
/// LOCALIZED BLV / FORWARDED slot / specpdl let-binding state is
/// observed. Mirrors `find_symbol_value` at GNU `src/data.c:1584-1609`.
/// See the extended comment on the identical helper in
/// `builtins/misc_eval.rs` (audit finding #3 in
/// `drafts/regex-search-audit.md`).
fn dynamic_or_global_symbol_value(eval: &super::eval::Context, name: &str) -> Option<Value> {
    let id = crate::emacs_core::intern::intern(name);
    eval.eval_symbol_by_id(id).ok()
}

pub(crate) fn builtin_replace_regexp_in_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-regexp-in-string", &args, 3, 7)?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|value| !value.is_nil())
        .unwrap_or(false);

    let fixedcase = args.get(3).is_some_and(|v| v.is_truthy());
    let literal = args.get(4).is_some_and(|v| v.is_truthy());
    let (subexp, _) = parse_replace_regexp_subexp_start_lisp(&args, expect_lisp_string(&args[2])?)?;

    if args[1].is_string() {
        let replacement = expect_lisp_string(&args[1])?.clone();
        return replace_regexp_in_string_lisp(&args, case_fold, |match_span, translated_md| {
            replace_match_on_substring(
                match_span,
                &replacement,
                fixedcase,
                literal,
                subexp,
                translated_md,
            )
        });
    }

    let func = args[1];
    let gc_roots = eval.save_specpdl_roots();
    eval.push_specpdl_root(func);
    let saved_match_data = eval.match_data.clone();

    let result = (|| -> EvalResult {
        replace_regexp_in_string_lisp(&args, case_fold, |match_span, translated_md| {
            // GNU wraps the whole function in `save-match-data`, but each REP
            // callback observes the translated substring-local match data.
            eval.match_data = translated_md.clone();
            let Some((match_start, match_end)) = translated_md
                .as_ref()
                .and_then(|md| md.groups.first().and_then(|group| *group))
            else {
                return Err(signal(
                    "error",
                    vec![
                        Value::string("replace-match subexpression does not exist"),
                        Value::fixnum(subexp as i64),
                    ],
                ));
            };
            let match_start_byte =
                super::regex::char_pos_to_byte_lisp_string(match_span, match_start);
            let match_end_byte = super::regex::char_pos_to_byte_lisp_string(match_span, match_end);
            let matched = match_span
                .slice(match_start_byte, match_end_byte)
                .expect("translated match bounds must slice");
            let func_result = eval.apply(func, vec![Value::heap_string(matched)])?;
            let replacement = func_result.as_lisp_string().ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), func_result],
                )
            })?;
            replace_match_on_substring(
                match_span,
                replacement,
                fixedcase,
                literal,
                subexp,
                translated_md,
            )
        })
    })();

    eval.match_data = saved_match_data;
    eval.restore_specpdl_roots(gc_roots);
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "search_test.rs"]
mod tests;
