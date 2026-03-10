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
use std::cell::RefCell;

thread_local! {
    static PURE_MATCH_DATA: RefCell<Option<super::regex::MatchData>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_int(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_integer_or_marker(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
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

fn preserve_case(replacement: &str, matched: &str) -> String {
    if matched.is_empty() || replacement.is_empty() {
        return replacement.to_string();
    }

    let all_upper = matched
        .chars()
        .all(|c| !c.is_alphabetic() || c.is_uppercase());
    let has_alpha = matched.chars().any(|c| c.is_alphabetic());
    if all_upper && has_alpha {
        return replacement.to_uppercase();
    }

    let mut chars = matched.chars();
    let first = chars.next().unwrap();
    let first_upper = first.is_uppercase();
    let rest_lower = chars.all(|c| !c.is_alphabetic() || c.is_lowercase());
    if first_upper && rest_lower {
        let mut out = String::with_capacity(replacement.len());
        let mut rep_chars = replacement.chars();
        if let Some(ch) = rep_chars.next() {
            for uc in ch.to_uppercase() {
                out.push(uc);
            }
        }
        out.extend(rep_chars);
        return out;
    }

    replacement.to_string()
}

fn expand_emacs_replacement(rep: &str, caps: &regex::Captures<'_>, literal: bool) -> String {
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
                if let Some(m) = caps.get(0) {
                    out.push_str(m.as_str());
                }
            }
            '1'..='9' => {
                let idx = next.to_digit(10).unwrap() as usize;
                if let Some(m) = caps.get(idx) {
                    out.push_str(m.as_str());
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
                flat.push(Value::Int(*start as i64));
                flat.push(Value::Int(*end as i64));
            }
            None => {
                flat.push(Value::Nil);
                flat.push(Value::Nil);
            }
        }
    }
    Value::list(flat)
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(string-match REGEXP STRING &optional START)` -- search for REGEXP in
/// STRING starting at START (default 0).  Returns the index of the match
/// or nil.  Updates match data.
pub(crate) fn builtin_string_match(args: Vec<Value>) -> EvalResult {
    expect_range_args("string-match", &args, 2, 4)?;
    let pattern = expect_string(&args[0])?;
    let s = expect_string(&args[1])?;
    let start = normalize_string_start_arg(&s, args.get(2))?;

    PURE_MATCH_DATA.with(|slot| {
        let mut md = slot.borrow_mut();
        match super::regex::string_match_full(&pattern, &s, start, &mut md) {
            // string_match_full returns a character position
            Ok(Some(char_pos)) => Ok(Value::Int(char_pos as i64)),
            Ok(None) => Ok(Value::Nil),
            Err(msg) => Err(signal("invalid-regexp", vec![Value::string(msg)])),
        }
    })
}

/// `(string-match-p REGEXP STRING &optional START)` -- like `string-match`
/// but does not modify match data.
pub(crate) fn builtin_string_match_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("string-match-p", &args, 2, 3)?;
    let pattern = expect_string(&args[0])?;
    let s = expect_string(&args[1])?;
    let start = normalize_string_start_arg(&s, args.get(2))?;

    // Emacs defaults `case-fold-search` to non-nil for string matching.
    let rust_pattern = format!("(?mi:{})", super::regex::translate_emacs_regex(&pattern));
    let re = regex::Regex::new(&rust_pattern)
        .map_err(|e| signal("invalid-regexp", vec![Value::string(e.to_string())]))?;

    let search_region = &s[start..];
    match re.find(search_region) {
        Some(m) => {
            let match_start = m.start() + start;
            Ok(Value::Int(s[..match_start].chars().count() as i64))
        }
        None => Ok(Value::Nil),
    }
}

/// `(regexp-quote STRING)` -- return a regexp that matches STRING literally,
/// quoting all special regex characters.
pub(crate) fn builtin_regexp_quote(args: Vec<Value>) -> EvalResult {
    expect_args("regexp-quote", &args, 1)?;
    let s = expect_string(&args[0])?;
    // Quote Emacs regex special characters.
    // In Emacs regex, the special characters that need quoting when used
    // literally are: . * + ? [ ^ $ \
    // Note: In Emacs, ( ) { } | are literal by default (their escaped
    // forms \( \) \{ \} \| are the special ones), so they do NOT need
    // quoting.
    let mut result = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '.' | '*' | '+' | '?' | '[' | '^' | '$' | '\\' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    Ok(Value::string(result))
}

/// `(match-beginning SUBEXP)` -- return the start position of the SUBEXPth
/// match group, or nil if unavailable.
pub(crate) fn builtin_match_beginning(args: Vec<Value>) -> EvalResult {
    expect_args("match-beginning", &args, 1)?;
    let subexp = expect_int(&args[0])? as usize;
    PURE_MATCH_DATA.with(|slot| {
        let md = slot.borrow();
        let Some(md) = md.as_ref() else {
            return Ok(Value::Nil);
        };
        match md.groups.get(subexp) {
            Some(Some((start, _end))) => {
                if md.searched_string.is_some() {
                    // String search: positions are already character positions
                    Ok(Value::Int(*start as i64))
                } else {
                    Ok(Value::Int(*start as i64))
                }
            }
            _ => Ok(Value::Nil),
        }
    })
}

/// `(match-end SUBEXP)` -- return the end position of the SUBEXPth
/// match group, or nil if unavailable.
pub(crate) fn builtin_match_end(args: Vec<Value>) -> EvalResult {
    expect_args("match-end", &args, 1)?;
    let subexp = expect_int(&args[0])? as usize;
    PURE_MATCH_DATA.with(|slot| {
        let md = slot.borrow();
        let Some(md) = md.as_ref() else {
            return Ok(Value::Nil);
        };
        match md.groups.get(subexp) {
            Some(Some((_start, end))) => {
                if md.searched_string.is_some() {
                    // String search: positions are already character positions
                    Ok(Value::Int(*end as i64))
                } else {
                    Ok(Value::Int(*end as i64))
                }
            }
            _ => Ok(Value::Nil),
        }
    })
}

/// `(match-data &optional INTEGERS REUSE RESEAT)` -- return the match data
/// as a list.
pub(crate) fn builtin_match_data(args: Vec<Value>) -> EvalResult {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("match-data"), Value::Int(args.len() as i64)],
        ));
    }
    PURE_MATCH_DATA.with(|slot| {
        let md = slot.borrow();
        let Some(md) = md.as_ref() else {
            return Ok(Value::Nil);
        };
        Ok(flatten_match_data(md))
    })
}

/// `(set-match-data LIST &optional RESEAT)` -- set match data from LIST.
pub(crate) fn builtin_set_match_data(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-match-data", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("set-match-data"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    if args[0].is_nil() {
        PURE_MATCH_DATA.with(|slot| {
            *slot.borrow_mut() = None;
        });
        return Ok(Value::Nil);
    }

    let items = list_to_vec(&args[0])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;

    let mut groups: Vec<Option<(usize, usize)>> = Vec::with_capacity(items.len() / 2);
    let mut i = 0usize;
    while i + 1 < items.len() {
        let start_v = &items[i];
        let end_v = &items[i + 1];
        if start_v.is_nil() && end_v.is_nil() {
            groups.push(None);
            i += 2;
            continue;
        }

        let start = expect_integer_or_marker(start_v)?;
        let end = expect_integer_or_marker(end_v)?;

        // Negative marker positions terminate match-data parsing.
        if start < 0 || end < 0 {
            break;
        }

        groups.push(Some((start as usize, end as usize)));
        i += 2;
    }

    PURE_MATCH_DATA.with(|slot| {
        if groups.is_empty() {
            *slot.borrow_mut() = None;
        } else {
            *slot.borrow_mut() = Some(super::regex::MatchData {
                groups,
                searched_string: None,
            });
        }
    });

    Ok(Value::Nil)
}

fn anchored_looking_at_matches(
    pattern: &str,
    text: &str,
) -> Result<Vec<Option<(usize, usize)>>, Flow> {
    let translated = super::regex::translate_emacs_regex(pattern);
    let anchored = if translated.starts_with("\\A") || translated.starts_with('^') {
        translated
    } else {
        format!("\\A(?:{translated})")
    };
    let re = regex::Regex::new(&format!("(?mi:{anchored})"))
        .map_err(|e| signal("invalid-regexp", vec![Value::string(e.to_string())]))?;

    match re.captures(text) {
        Some(caps) => {
            let mut groups = Vec::with_capacity(caps.len());
            for i in 0..caps.len() {
                groups.push(caps.get(i).map(|m| (m.start(), m.end())));
            }
            Ok(groups)
        }
        None => Ok(Vec::new()),
    }
}

/// `(looking-at REGEXP)` -- test whether text after point matches REGEXP.
/// In batch mode we support an optional second argument as a sample string.
/// When absent, this returns nil after validating REGEXP.
pub(crate) fn builtin_looking_at(args: Vec<Value>) -> EvalResult {
    expect_range_args("looking-at", &args, 1, 2)?;
    let pattern = expect_string(&args[0])?;

    let text = args.get(1).and_then(|value| value.as_str());
    match text {
        Some(text) => match anchored_looking_at_matches(&pattern, text)? {
            groups if groups.is_empty() => {
                PURE_MATCH_DATA.with(|slot| *slot.borrow_mut() = None);
                Ok(Value::Nil)
            }
            groups => {
                PURE_MATCH_DATA.with(|slot| {
                    *slot.borrow_mut() = Some(super::regex::MatchData {
                        groups,
                        searched_string: Some(text.to_string()),
                    })
                });
                Ok(Value::True)
            }
        },
        None => {
            let _ = anchored_looking_at_matches(&pattern, "")?;
            PURE_MATCH_DATA.with(|slot| *slot.borrow_mut() = None);
            Ok(Value::Nil)
        }
    }
}

/// `(looking-at-p REGEXP)` -- same as `looking-at`, preserving match data.
pub(crate) fn builtin_looking_at_p(args: Vec<Value>) -> EvalResult {
    expect_args("looking-at-p", &args, 1)?;
    let pattern = expect_string(&args[0])?;
    PURE_MATCH_DATA.with(|slot| {
        let snapshot = slot.borrow().clone();
        let _ = anchored_looking_at_matches(&pattern, "")?;
        *slot.borrow_mut() = snapshot;
        Ok(Value::Nil)
    })
}

/// `(replace-regexp-in-string REGEXP REP STRING &optional FIXEDCASE LITERAL SUBEXP START)`
/// -- replace all matches of REGEXP in STRING with REP.
/// REP is a string (with `\&` and `\N` back-references) or, in the pure
/// variant, only a string.
pub(crate) fn builtin_replace_regexp_in_string(args: Vec<Value>) -> EvalResult {
    expect_range_args("replace-regexp-in-string", &args, 3, 7)?;
    // Pure variant: REP must be a string.
    let rep = expect_string(&args[1])?;
    replace_regexp_in_string_core(&args, true, &rep, None)
}

/// Parse SUBEXP and START args (positions 5 and 6) for replace-regexp-in-string.
fn parse_replace_regexp_subexp_start(args: &[Value], s: &str) -> Result<(i64, usize), Flow> {
    // args[5] = SUBEXP (optional), args[6] = START (optional)
    let subexp = match args.get(5) {
        Some(Value::Nil) | None => 0i64,
        Some(value) => expect_int(value)?,
    };
    if subexp < 0 {
        return Err(signal(
            "args-out-of-range",
            vec![
                Value::Int(subexp),
                Value::Int(0),
                Value::Int(s.len() as i64),
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
            Some(Value::Nil) | None => 0i64,
            Some(value) => expect_int(value)?,
        };
        (sub, so)
    } else {
        parse_replace_regexp_subexp_start(args, &s)?
    };

    let translated = super::regex::translate_emacs_regex(&pattern);
    let rust_pattern = if case_fold {
        format!("(?mi:{translated})")
    } else {
        format!("(?m:{translated})")
    };
    let re = regex::Regex::new(&rust_pattern)
        .map_err(|e| signal("invalid-regexp", vec![Value::string(e.to_string())]))?;

    let max_subexp = re.captures_len().saturating_sub(1);
    if (subexp as usize) > max_subexp {
        return Err(signal(
            "error",
            vec![
                Value::string("replace-match subexpression does not exist"),
                Value::Int(subexp),
            ],
        ));
    }

    let search_region = &s[start..];
    let mut out = String::with_capacity(search_region.len());
    let mut cursor = 0usize;

    for caps in re.captures_iter(search_region) {
        let full_match = match caps.get(0) {
            Some(m) => m,
            None => continue,
        };

        let (replace_start, replace_end, case_source) = if subexp == 0 {
            let src = full_match.as_str();
            (full_match.start(), full_match.end(), src)
        } else if let Some(g) = caps.get(subexp as usize) {
            let src = g.as_str();
            (g.start(), g.end(), src)
        } else {
            return Err(signal(
                "error",
                vec![
                    Value::string("replace-match subexpression does not exist"),
                    Value::Int(subexp),
                ],
            ));
        };

        out.push_str(&search_region[cursor..replace_start]);
        let base = expand_emacs_replacement(rep, &caps, literal);
        let replacement = if fixedcase {
            base
        } else {
            preserve_case(&base, case_source)
        };
        out.push_str(&replacement);
        cursor = replace_end;
    }

    out.push_str(&search_region[cursor..]);
    Ok(Value::string(out))
}

fn dynamic_or_global_symbol_value(eval: &super::eval::Evaluator, name: &str) -> Option<Value> {
    let name_id = intern(name);
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }
    eval.obarray.symbol_value(name).cloned()
}

pub(crate) fn builtin_replace_regexp_in_string_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-regexp-in-string", &args, 3, 7)?;
    let case_fold = dynamic_or_global_symbol_value(eval, "case-fold-search")
        .map(|value| !value.is_nil())
        .unwrap_or(true);

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

    let translated = super::regex::translate_emacs_regex(&pattern);
    let rust_pattern = if case_fold {
        format!("(?mi:{translated})")
    } else {
        format!("(?m:{translated})")
    };
    let re = regex::Regex::new(&rust_pattern)
        .map_err(|e| signal("invalid-regexp", vec![Value::string(e.to_string())]))?;

    let max_subexp = re.captures_len().saturating_sub(1);
    if (subexp as usize) > max_subexp {
        return Err(signal(
            "error",
            vec![
                Value::string("replace-match subexpression does not exist"),
                Value::Int(subexp),
            ],
        ));
    }

    let search_region = s[start..].to_string();
    let mut out = String::with_capacity(search_region.len());
    let mut cursor = 0usize;

    // Collect all matches first to avoid borrow issues
    let all_caps: Vec<_> = re.captures_iter(&search_region).collect();

    let saved = eval.save_temp_roots();
    eval.push_temp_root(func);

    for caps in &all_caps {
        let full_match = match caps.get(0) {
            Some(m) => m,
            None => continue,
        };

        let (replace_start, replace_end, case_source) = if subexp == 0 {
            let src = full_match.as_str().to_string();
            (full_match.start(), full_match.end(), src)
        } else if let Some(g) = caps.get(subexp as usize) {
            let src = g.as_str().to_string();
            (g.start(), g.end(), src)
        } else {
            eval.restore_temp_roots(saved);
            return Err(signal(
                "error",
                vec![
                    Value::string("replace-match subexpression does not exist"),
                    Value::Int(subexp),
                ],
            ));
        };

        // Set match-data so the function can call match-string etc.
        // In Emacs, replace-regexp-in-string calls string-match on the
        // whole STRING with START, so match positions are character
        // positions relative to the whole string.
        let prefix_chars = s[..start].chars().count();
        let mut groups = Vec::with_capacity(caps.len());
        for i in 0..caps.len() {
            groups.push(caps.get(i).map(|m| {
                let cs = search_region[..m.start()].chars().count() + prefix_chars;
                let ce = search_region[..m.end()].chars().count() + prefix_chars;
                (cs, ce)
            }));
        }
        eval.match_data = Some(super::regex::MatchData {
            groups,
            searched_string: Some(s.clone()),
        });

        out.push_str(&search_region[cursor..replace_start]);

        // Call the function with the matched string
        let matched_str = full_match.as_str();
        let func_result = eval.apply(func, vec![Value::string(matched_str)])?;
        let base = match func_result.as_str() {
            Some(s) => s.to_string(),
            None => {
                eval.restore_temp_roots(saved);
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
        cursor = replace_end;
    }

    eval.restore_temp_roots(saved);
    out.push_str(&search_region[cursor..]);
    Ok(Value::string(out))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "search_test.rs"]
mod tests;
