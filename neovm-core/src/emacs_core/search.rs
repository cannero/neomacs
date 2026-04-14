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
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_lisp_string(val: &Value) -> Result<&'static crate::heap_types::LispString, Flow> {
    val.as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )
    })
}

fn cloned_lisp_string_value(string: &crate::heap_types::LispString) -> Value {
    Value::heap_string(string.clone())
}

fn regexp_quote_lisp_string(input: &crate::heap_types::LispString) -> crate::heap_types::LispString {
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

fn preserve_case(replacement: &str, matched: &str) -> String {
    super::casefiddle::apply_replace_match_case(replacement, matched)
}

fn expand_emacs_replacement(
    rep: &str,
    groups: &[Option<(usize, usize)>],
    source: &str,
    literal: bool,
) -> String {
    if literal {
        return rep.to_string();
    }

    let mut out = String::with_capacity(rep.len());
    let mut chars = rep.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let Some(next) = chars.next() else {
            out.push('\\');
            break;
        };

        match next {
            '&' => {
                if let Some(Some((start, end))) = groups.first()
                    && let Some(text) = source.get(*start..*end)
                {
                    out.push_str(text);
                }
            }
            '1'..='9' => {
                let idx = next.to_digit(10).unwrap() as usize;
                if let Some(Some((start, end))) = groups.get(idx)
                    && let Some(text) = source.get(*start..*end)
                {
                    out.push_str(text);
                }
            }
            '\\' => out.push('\\'),
            other => out.push(other),
        }
    }

    out
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

/// Parse SUBEXP and START args (positions 5 and 6) for replace-regexp-in-string.
fn parse_replace_regexp_subexp_start(args: &[Value], s: &str) -> Result<(i64, usize), Flow> {
    // args[5] = SUBEXP (optional), args[6] = START (optional)
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
                Value::fixnum(s.len() as i64),
            ],
        ));
    }
    let start = normalize_string_start_arg(s, args.get(6))?;
    Ok((subexp, start))
}

/// Core implementation for replace-regexp-in-string with string replacement.
fn replace_regexp_in_string_core(
    args: &[Value],
    case_fold: bool,
    rep: &str,
    start_override: Option<usize>,
) -> EvalResult {
    let pattern = expect_string(&args[0])?;
    let s = expect_string(&args[2])?;
    let fixedcase = args.get(3).is_some_and(|v| v.is_truthy());
    let literal = args.get(4).is_some_and(|v| v.is_truthy());

    let (subexp, start) = if let Some(so) = start_override {
        let sub = match args.get(5) {
            Some(v) if v.is_nil() => 0i64,
            None => 0i64,
            Some(value) => expect_int(value)?,
        };
        (sub, so)
    } else {
        parse_replace_regexp_subexp_start(args, &s)?
    };

    let iterated =
        super::regex::iterate_string_matches_with_case_fold(&pattern, &s, start, case_fold)
            .map_err(|msg| signal("invalid-regexp", vec![Value::string(msg)]))?;

    let max_subexp = iterated.capture_count.saturating_sub(1);
    if (subexp as usize) > max_subexp {
        return Err(signal(
            "error",
            vec![
                Value::string("replace-match subexpression does not exist"),
                Value::fixnum(subexp),
            ],
        ));
    }

    let mut out = String::with_capacity(s.len().saturating_sub(start));
    let mut cursor = start;

    for groups in iterated.matches {
        let Some((_, full_end)) = groups.first().and_then(|group| *group) else {
            continue;
        };
        let (replace_start, replace_end, case_source) = if subexp == 0 {
            let Some((match_start, match_end)) = groups.first().and_then(|group| *group) else {
                continue;
            };
            let Some(src) = s.get(match_start..match_end) else {
                continue;
            };
            (match_start, match_end, src)
        } else if let Some(Some((group_start, group_end))) = groups.get(subexp as usize) {
            let Some(src) = s.get(*group_start..*group_end) else {
                continue;
            };
            (*group_start, *group_end, src)
        } else {
            return Err(signal(
                "error",
                vec![
                    Value::string("replace-match subexpression does not exist"),
                    Value::fixnum(subexp),
                ],
            ));
        };

        out.push_str(&s[cursor..replace_start]);
        let base = expand_emacs_replacement(rep, &groups, &s, literal);
        let replacement = if fixedcase {
            base
        } else {
            preserve_case(&base, case_source)
        };
        out.push_str(&replacement);
        cursor = if subexp == 0 { full_end } else { replace_end };
    }

    out.push_str(&s[cursor..]);
    Ok(Value::string(out))
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

    // Check if REP is a string or a function
    let rep_is_string = args[1].as_str().is_some();

    if rep_is_string {
        let rep = expect_string(&args[1])?;
        return replace_regexp_in_string_core(&args, case_fold, &rep, None);
    }

    // REP is a function — call it with each matched string.
    let func = args[1];
    let pattern = expect_string(&args[0])?;
    let s = expect_string(&args[2])?;
    let fixedcase = args.get(3).is_some_and(|v| v.is_truthy());
    let _literal = args.get(4).is_some_and(|v| v.is_truthy());
    let (subexp, start) = parse_replace_regexp_subexp_start(&args, &s)?;

    let iterated =
        super::regex::iterate_string_matches_with_case_fold(&pattern, &s, start, case_fold)
            .map_err(|msg| signal("invalid-regexp", vec![Value::string(msg)]))?;

    let max_subexp = iterated.capture_count.saturating_sub(1);
    if (subexp as usize) > max_subexp {
        return Err(signal(
            "error",
            vec![
                Value::string("replace-match subexpression does not exist"),
                Value::fixnum(subexp),
            ],
        ));
    }

    let search_region = &s[start..];
    let mut out = String::with_capacity(search_region.len());
    let mut cursor = start;
    let prefix_chars = s[..start].chars().count();
    let searched_string = match args[2].kind() {
        ValueKind::String => super::regex::SearchedString::Heap(args[2]),
        _ => super::regex::SearchedString::Owned(crate::heap_types::LispString::from_utf8(&s)),
    };

    eval.with_gc_scope_result(|ctx| {
        ctx.root(func);

        for groups in &iterated.matches {
            let Some((full_start, full_end)) = groups.first().and_then(|group| *group) else {
                continue;
            };

            let (replace_start, replace_end, case_source) = if subexp == 0 {
                let Some(src) = s.get(full_start..full_end) else {
                    continue;
                };
                (full_start, full_end, src.to_string())
            } else if let Some(Some((group_start, group_end))) = groups.get(subexp as usize) {
                let Some(src) = s.get(*group_start..*group_end) else {
                    continue;
                };
                (*group_start, *group_end, src.to_string())
            } else {
                return Err(signal(
                    "error",
                    vec![
                        Value::string("replace-match subexpression does not exist"),
                        Value::fixnum(subexp),
                    ],
                ));
            };

            // Set match-data so the function can call match-string etc.
            // In Emacs, replace-regexp-in-string calls string-match on the
            // whole STRING with START, so match positions are character
            // positions relative to the whole string.
            let mut match_groups = Vec::with_capacity(groups.len());
            for group in groups {
                match_groups.push(group.map(|(group_start, group_end)| {
                    let cs = search_region[..group_start - start].chars().count() + prefix_chars;
                    let ce = search_region[..group_end - start].chars().count() + prefix_chars;
                    (cs, ce)
                }));
            }
            ctx.match_data = Some(super::regex::MatchData {
                groups: match_groups,
                searched_string: Some(searched_string.clone()),
                searched_buffer: None,
            });

            out.push_str(&s[cursor..replace_start]);

            // Call the function with the matched string
            let matched_str = &s[full_start..full_end];
            let func_result = ctx.apply(func, vec![Value::string(matched_str)])?;
            let base = match func_result.as_str() {
                Some(s) => s.to_string(),
                None => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("stringp"), func_result],
                    ));
                }
            };

            let replacement = if fixedcase {
                base
            } else {
                preserve_case(&base, &case_source)
            };
            out.push_str(&replacement);
            cursor = if subexp == 0 { full_end } else { replace_end };
        }

        out.push_str(&s[cursor..]);
        Ok(Value::string(out))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "search_test.rs"]
mod tests;
