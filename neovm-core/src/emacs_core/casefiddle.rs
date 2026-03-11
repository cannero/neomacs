//! Case conversion and character builtins.
//!
//! Implements `capitalize`, `upcase-initials`, and `char-resolve-modifiers`.

use super::error::{EvalResult, Flow, signal};
use super::value::*;

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

// ---------------------------------------------------------------------------
// Character helpers
// ---------------------------------------------------------------------------

const CHAR_META: i64 = 0x8000000;
const CHAR_CTL: i64 = 0x4000000;
const CHAR_SHIFT: i64 = 0x2000000;
const CHAR_HYPER: i64 = 0x1000000;
const CHAR_SUPER: i64 = 0x0800000;
const CHAR_ALT: i64 = 0x0400000;
const CHAR_MODIFIER_MASK: i64 =
    CHAR_META | CHAR_CTL | CHAR_SHIFT | CHAR_HYPER | CHAR_SUPER | CHAR_ALT;

/// Convert a character code to a Rust char (if it's a valid Unicode scalar value).
fn code_to_char(code: i64) -> Option<char> {
    if (0..=0x10FFFF).contains(&code) {
        char::from_u32(code as u32)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Case conversion helpers
// ---------------------------------------------------------------------------

/// Uppercase a single character code, returning the new code.
fn upcase_char(code: i64) -> i64 {
    if preserve_casefiddle_upcase_payload(code) {
        return code;
    }
    match code {
        223 => return 7838,
        452 | 497 => return code + 1,
        454 | 457 | 460 | 499 => return code - 1,
        455 | 458 => return code + 1,
        8064..=8071 | 8080..=8087 | 8096..=8103 => return code + 8,
        8115 | 8131 | 8179 => return code + 9,
        _ => {}
    }
    match code_to_char(code) {
        Some(c) => {
            let mut upper = c.to_uppercase();
            // to_uppercase() may yield multiple chars (e.g. German eszett);
            // take only the first to stay consistent with Emacs behavior.
            upper.next().map(|u| u as i64).unwrap_or(code)
        }
        None => code,
    }
}

fn preserve_casefiddle_upcase_payload(code: i64) -> bool {
    matches!(
        code,
        329
            | 411
            | 453
            | 456
            | 459
            | 496
            | 498
            | 612
            | 912
            | 944
            | 1415
            | 4304..=4346
            | 4349..=4351
            | 7306
            | 7830..=7834
            | 8016
            | 8018
            | 8020
            | 8022
            | 8072..=8079
            | 8088..=8095
            | 8104..=8111
            | 8114
            | 8116
            | 8118..=8119
            | 8124
            | 8130
            | 8132
            | 8134..=8135
            | 8140
            | 8146..=8147
            | 8150..=8151
            | 8162..=8164
            | 8166..=8167
            | 8178
            | 8180
            | 8182..=8183
            | 8188
            | 42957
            | 42959
            | 42963
            | 42965
            | 42971
            | 64256..=64262
            | 64275..=64279
            | 68976..=68997
            | 93883..=93907
    )
}

fn titlecase_from_uppercase_expansion(expansion: &[char]) -> String {
    let mut result = String::new();
    let mut seen_cased = false;

    for uc in expansion {
        let is_cased = uc.is_uppercase() || uc.is_lowercase();
        if !seen_cased {
            result.push(*uc);
            if is_cased {
                seen_cased = true;
            }
            continue;
        }

        if is_cased {
            for lc in uc.to_lowercase() {
                result.push(lc);
            }
        } else {
            result.push(*uc);
        }
    }

    result
}

fn titlecase_combining_iota_override(code: i64) -> Option<&'static str> {
    match code {
        8114 => Some("\u{1FBA}\u{0345}"),
        8116 => Some("\u{0386}\u{0345}"),
        8119 => Some("\u{0391}\u{0342}\u{0345}"),
        8130 => Some("\u{1FCA}\u{0345}"),
        8132 => Some("\u{0389}\u{0345}"),
        8135 => Some("\u{0397}\u{0342}\u{0345}"),
        8178 => Some("\u{1FFA}\u{0345}"),
        8180 => Some("\u{038F}\u{0345}"),
        8183 => Some("\u{03A9}\u{0342}\u{0345}"),
        _ => None,
    }
}

fn titlecase_uses_precomposed_upcase(code: i64) -> bool {
    matches!(
        code,
        8064..=8071
            | 8072..=8111
            | 8115
            | 8124
            | 8131
            | 8140
            | 8179
            | 8188
    )
}

fn titlecase_word_initial(c: char) -> String {
    let code = c as i64;
    if let Some(explicit) = titlecase_combining_iota_override(code) {
        return explicit.to_string();
    }

    let expansion: Vec<char> = c.to_uppercase().collect();
    if expansion.len() > 1 && !titlecase_uses_precomposed_upcase(code) {
        return titlecase_from_uppercase_expansion(&expansion);
    }

    if let Some(mapped) = code_to_char(upcase_char(code)) {
        mapped.to_string()
    } else {
        c.to_uppercase().collect()
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(capitalize OBJ)` -- if OBJ is a string, capitalize the first letter
/// (uppercase first, lowercase rest).  If OBJ is a character, uppercase it.
pub(crate) fn builtin_capitalize(args: Vec<Value>) -> EvalResult {
    expect_args("capitalize", &args, 1)?;
    match &args[0] {
        Value::Str(_) => {
            let s = args[0].as_str().unwrap();
            let capitalized = capitalize_string(s);
            Ok(Value::string(capitalized))
        }
        Value::Char(c) => {
            let code = *c as i64;
            Ok(Value::Int(upcase_char(code)))
        }
        Value::Int(n) => Ok(Value::Int(upcase_char(*n))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-or-string-p"), *other],
        )),
    }
}

/// Capitalize a string: uppercase the first letter of each word,
/// lowercase the rest.  A "word" starts after any non-alphanumeric character.
fn capitalize_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut new_word = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if new_word {
                for u in titlecase_word_initial(c).chars() {
                    result.push(u);
                }
                new_word = false;
            } else {
                for l in c.to_lowercase() {
                    result.push(l);
                }
            }
        } else {
            result.push(c);
            new_word = true;
        }
    }
    result
}

/// `(upcase-initials OBJ)` -- uppercase the first letter of each word in
/// a string, leaving the rest unchanged.  For a char, uppercase it.
pub(crate) fn builtin_upcase_initials(args: Vec<Value>) -> EvalResult {
    expect_args("upcase-initials", &args, 1)?;
    match &args[0] {
        Value::Str(_) => {
            let s = args[0].as_str().unwrap();
            let result = upcase_initials_string(s);
            Ok(Value::string(result))
        }
        Value::Char(c) => {
            let code = *c as i64;
            Ok(Value::Int(upcase_char(code)))
        }
        Value::Int(n) => Ok(Value::Int(upcase_char(*n))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-or-string-p"), *other],
        )),
    }
}

/// Uppercase the first letter of each word, leaving the rest unchanged.
pub(crate) fn upcase_initials_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut new_word = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if new_word {
                for u in titlecase_word_initial(c).chars() {
                    result.push(u);
                }
                new_word = false;
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
            new_word = true;
        }
    }
    result
}

pub(crate) fn apply_replace_match_case(replacement: &str, matched: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CaseAction {
        NoChange,
        AllCaps,
        CapInitial,
    }

    let mut some_multiletter_word = false;
    let mut some_lowercase = false;
    let mut some_uppercase = false;
    let mut some_nonuppercase_initial = false;
    let mut prev_is_word = false;

    for ch in matched.chars() {
        if ch.is_lowercase() {
            some_lowercase = true;
            if prev_is_word {
                some_multiletter_word = true;
            } else {
                some_nonuppercase_initial = true;
            }
        } else if ch.is_uppercase() {
            some_uppercase = true;
            if prev_is_word {
                some_multiletter_word = true;
            }
        } else if !prev_is_word {
            some_nonuppercase_initial = true;
        }

        prev_is_word = ch.is_alphanumeric();
    }

    let case_action = if !some_lowercase && some_multiletter_word {
        CaseAction::AllCaps
    } else if !some_nonuppercase_initial && some_multiletter_word {
        CaseAction::CapInitial
    } else if !some_nonuppercase_initial && some_uppercase {
        CaseAction::AllCaps
    } else {
        CaseAction::NoChange
    };

    match case_action {
        CaseAction::NoChange => replacement.to_string(),
        CaseAction::AllCaps => replacement.to_uppercase(),
        CaseAction::CapInitial => upcase_initials_string(replacement),
    }
}

/// `(char-resolve-modifiers CHAR)` -- resolve modifier bits in character.
/// Resolve shift/control modifiers into the base character where possible.
pub(crate) fn builtin_char_resolve_modifiers(args: Vec<Value>) -> EvalResult {
    expect_args("char-resolve-modifiers", &args, 1)?;

    let code = match &args[0] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *other],
            ));
        }
    };

    let modifiers = code & CHAR_MODIFIER_MASK;
    let mut base = code & !CHAR_MODIFIER_MASK;
    let mut remaining_mods = modifiers;

    if remaining_mods & CHAR_SHIFT != 0 {
        if base >= 'a' as i64 && base <= 'z' as i64 {
            base = base - 'a' as i64 + 'A' as i64;
            remaining_mods &= !CHAR_SHIFT;
        } else if base >= 'A' as i64 && base <= 'Z' as i64 {
            remaining_mods &= !CHAR_SHIFT;
        }
    }

    if remaining_mods & CHAR_CTL != 0 {
        if base >= '@' as i64 && base <= '_' as i64 {
            base &= 0x1F;
            remaining_mods &= !CHAR_CTL;
        } else if base >= 'a' as i64 && base <= 'z' as i64 {
            base &= 0x1F;
            remaining_mods &= !CHAR_CTL;
        } else if base == '?' as i64 {
            base = 127;
            remaining_mods &= !CHAR_CTL;
        }
    }

    Ok(Value::Int(base | remaining_mods))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "casefiddle_test.rs"]
mod tests;
