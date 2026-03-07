//! Character encoding, multibyte support, and character utilities.
//!
//! Neomacs uses UTF-8 internally.  This module provides Emacs-compatible
//! character classification, width calculation, and encoding conversion
//! APIs.

use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::string_escape::{
    bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage, storage_byte_len,
};
use crate::emacs_core::value::{StringTextPropertyRun, Value, with_heap};

const MAX_CHAR_CODE: i64 = 0x3F_FFFF;
const RAW_BYTE_SENTINEL_BASE: u32 = 0xE000;
const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_BASE: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;

// ---------------------------------------------------------------------------
// Character classification
// ---------------------------------------------------------------------------

/// Character width for display purposes (East Asian width).
pub fn char_width(c: char) -> usize {
    let cp = c as u32;
    // Control characters with dedicated rendering widths.
    if cp == 0x09 {
        return 8; // TAB advances to tab stop
    }
    if cp == 0x0a {
        return 0; // NEWLINE has zero display width
    }
    if cp < 0x20 || cp == 0x7f {
        return 2; // ^X notation
    }
    if (0x80..=0x9f).contains(&cp) {
        return 4; // octal escaped control bytes
    }
    // Non-spacing marks
    if is_zero_width(c) {
        return 0;
    }
    // Wide characters (CJK, etc.)
    if is_wide_char(c) {
        return 2;
    }
    1
}

/// Whether the character is zero-width (combining mark, etc.).
fn is_zero_width(c: char) -> bool {
    let cp = c as u32;
    // Common combining mark ranges
    (0x0300..=0x036f).contains(&cp) // Combining Diacriticals
        || (0x0483..=0x0489).contains(&cp) // Cyrillic combining
        || (0x0591..=0x05bd).contains(&cp) // Hebrew
        || (0x0610..=0x061a).contains(&cp) // Arabic
        || (0x064b..=0x065f).contains(&cp)
        || (0x0670..=0x0670).contains(&cp)
        || (0x06d6..=0x06dc).contains(&cp)
        || (0x0730..=0x074a).contains(&cp) // Syriac
        || (0x0900..=0x0903).contains(&cp) // Devanagari
        || (0x093a..=0x094f).contains(&cp)
        || (0x0e31..=0x0e3a).contains(&cp) // Thai
        || (0x0e47..=0x0e4e).contains(&cp)
        || (0x1160..=0x11ff).contains(&cp) // Hangul jungseong/jongseong
        || (0x200b..=0x200f).contains(&cp) // Zero-width space, ZWNJ, ZWJ
        || (0x202a..=0x202e).contains(&cp) // Bidi control
        || (0x2060..=0x2064).contains(&cp) // Invisible operators
        || (0xfe00..=0xfe0f).contains(&cp) // Variation selectors
        || (0xfe20..=0xfe2f).contains(&cp) // Combining half marks
        || (0xfeff..=0xfeff).contains(&cp) // BOM
        || (0x1d167..=0x1d169).contains(&cp) // Musical combining
        || (0x1d173..=0x1d182).contains(&cp)
        || (0xe0020..=0xe007f).contains(&cp) // Tags
        || (0xe0100..=0xe01ef).contains(&cp) // Variation selectors supplement
}

/// Whether the character is full-width (East Asian wide).
fn is_wide_char(c: char) -> bool {
    let cp = c as u32;
    // CJK Unified Ideographs
    (0x1100..=0x115f).contains(&cp) // Hangul Jamo
        || (0x2e80..=0x303e).contains(&cp) // CJK Radicals, Kangxi, etc.
        || (0x3040..=0x33bf).contains(&cp) // Hiragana, Katakana, CJK compat
        || (0x3400..=0x4dbf).contains(&cp) // CJK Extension A
        || (0x4e00..=0x9fff).contains(&cp) // CJK Unified Ideographs
        || (0xa000..=0xa4cf).contains(&cp) // Yi
        || (0xac00..=0xd7a3).contains(&cp) // Hangul Syllables
        || (0xf900..=0xfaff).contains(&cp) // CJK Compatibility Ideographs
        || (0xfe10..=0xfe19).contains(&cp) // Vertical forms
        || (0xfe30..=0xfe6b).contains(&cp) // CJK Compatibility Forms
        || (0xff01..=0xff60).contains(&cp) // Fullwidth forms
        || (0xffe0..=0xffe6).contains(&cp) // Fullwidth signs
        || (0x1f200..=0x1f2ff).contains(&cp) // Enclosed ideographic
        || (0x1f300..=0x1f9ff).contains(&cp) // Emoji (most are wide)
        || (0x20000..=0x2ffff).contains(&cp) // CJK Extension B-F
        || (0x30000..=0x3ffff).contains(&cp) // CJK Extension G+
}

/// String display width (sum of char widths).
pub fn string_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Whether a character is printable (not a control char).
pub fn is_printable(c: char) -> bool {
    let cp = c as u32;
    cp >= 0x20 && cp != 0x7f && !(0x80..=0x9f).contains(&cp)
}

/// Whether a character is a whitespace character.
pub fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// Whether a character is a word constituent (alphanumeric + underscore).
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a string is all ASCII.
pub fn is_ascii_string(s: &str) -> bool {
    s.bytes().all(|b| b < 128)
}

/// Whether a string is multibyte (contains non-ASCII).
pub fn is_multibyte_string(s: &str) -> bool {
    s.chars().any(|ch| {
        let cp = ch as u32;
        cp > 0x7f && !(UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp)
    })
}

// ---------------------------------------------------------------------------
// Encoding conversion
// ---------------------------------------------------------------------------

/// Encode a string to bytes using the specified coding system.
/// Currently only UTF-8 is supported.
pub fn encode_string(s: &str, coding_system: &str) -> Vec<u8> {
    match coding_system {
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac" => s.as_bytes().to_vec(),
        "latin-1" | "iso-8859-1" | "iso-latin-1" => s
            .chars()
            .map(|c| if (c as u32) <= 0xff { c as u8 } else { b'?' })
            .collect(),
        "ascii" | "us-ascii" => s
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect(),
        _ => s.as_bytes().to_vec(), // default to UTF-8
    }
}

/// Decode bytes to a string using the specified coding system.
/// Currently only UTF-8 is supported.
pub fn decode_bytes(bytes: &[u8], coding_system: &str) -> String {
    match coding_system {
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac" => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" => bytes.iter().map(|&b| b as char).collect(),
        "ascii" | "us-ascii" => bytes
            .iter()
            .map(|&b| if b < 128 { b as char } else { '?' })
            .collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn storage_string_to_bytes(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if cp <= 0x7F {
            bytes.push(cp as u8);
            continue;
        }
        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - RAW_BYTE_SENTINEL_BASE) as u8);
            continue;
        }
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - UNIBYTE_BYTE_SENTINEL_BASE) as u8);
            continue;
        }

        let mut utf8 = [0u8; 4];
        let encoded = ch.encode_utf8(&mut utf8);
        bytes.extend_from_slice(encoded.as_bytes());
    }
    bytes
}

fn bytes_to_multibyte_raw_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        if *b <= 0x7F {
            out.push(*b as char);
            continue;
        }
        if let Some(encoded) = encode_nonunicode_char_for_storage(0x3FFF00 + (*b as u32)) {
            out.push_str(&encoded);
            continue;
        }
        out.push('\u{FFFD}');
    }
    out
}

fn charset_property_runs(text: &str, charset: &str) -> Vec<StringTextPropertyRun> {
    let mut runs = Vec::new();
    let mut start = None;
    let mut char_idx = 0usize;

    for ch in text.chars() {
        if (ch as u32) > 0x7f {
            start.get_or_insert(char_idx);
        } else if let Some(run_start) = start.take() {
            runs.push(StringTextPropertyRun {
                start: run_start,
                end: char_idx,
                plist: Value::list(vec![Value::symbol("charset"), Value::symbol(charset)]),
            });
        }
        char_idx += 1;
    }

    if let Some(run_start) = start {
        runs.push(StringTextPropertyRun {
            start: run_start,
            end: char_idx,
            plist: Value::list(vec![Value::symbol("charset"), Value::symbol(charset)]),
        });
    }

    runs
}

// ---------------------------------------------------------------------------
// Byte/char position conversion
// ---------------------------------------------------------------------------

/// Convert character position to byte position in a UTF-8 string.
pub fn char_to_byte_pos(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(byte_pos, _)| byte_pos)
        .unwrap_or(s.len())
}

/// Convert byte position to character position in a UTF-8 string.
pub fn byte_to_char_pos(s: &str, byte_pos: usize) -> usize {
    s[..byte_pos.min(s.len())].chars().count()
}

// ---------------------------------------------------------------------------
// Glyphless character representation
// ---------------------------------------------------------------------------

/// How to display a glyphless (control/non-printable) character.
pub fn glyphless_char_display(c: char) -> String {
    let cp = c as u32;
    if cp < 0x20 {
        format!("^{}", (cp + 0x40) as u8 as char)
    } else if cp == 0x7f {
        "^?".to_string()
    } else if cp < 0x100 {
        format!("\\{:03o}", cp)
    } else if cp < 0x10000 {
        format!("\\u{:04X}", cp)
    } else {
        format!("\\U{:08X}", cp)
    }
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

use crate::emacs_core::error::{EvalResult, signal};

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), crate::emacs_core::error::Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(
    name: &str,
    args: &[Value],
    min: usize,
) -> Result<(), crate::emacs_core::error::Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(val: &Value) -> Result<String, crate::emacs_core::error::Flow> {
    match val {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn known_coding_system(name: &str) -> bool {
    crate::emacs_core::coding::CodingSystemManager::new().is_known(name)
}

/// `(char-width CHAR)` -> integer
pub(crate) fn builtin_char_width(args: Vec<Value>) -> EvalResult {
    expect_args("char-width", &args, 1)?;
    let code = match &args[0] {
        Value::Char(c) => *c as i64,
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ));
        }
    };
    if !(0..=MAX_CHAR_CODE).contains(&code) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::Int(code)],
        ));
    }
    // Non-Unicode char codes still have width 1 in Emacs.
    if code > 0x10_FFFF {
        return Ok(Value::Int(1));
    }
    let width = char::from_u32(code as u32).map(char_width).unwrap_or(1);
    Ok(Value::Int(width as i64))
}

/// `(string-bytes STRING)` -> integer byte length of STRING.
pub(crate) fn builtin_string_bytes(args: Vec<Value>) -> EvalResult {
    expect_args("string-bytes", &args, 1)?;
    let s = expect_string(&args[0])?;
    Ok(Value::Int(storage_byte_len(&s) as i64))
}

/// `(multibyte-string-p STRING)` -> t or nil
pub(crate) fn builtin_multibyte_string_p(args: Vec<Value>) -> EvalResult {
    expect_args("multibyte-string-p", &args, 1)?;
    match &args[0] {
        Value::Str(id) => Ok(Value::bool(with_heap(|h| h.string_is_multibyte(*id)))),
        _ => Ok(Value::Nil),
    }
}

/// `(unibyte-string-p STRING)` -> t or nil
#[cfg(test)]
pub(crate) fn builtin_unibyte_string_p(args: Vec<Value>) -> EvalResult {
    expect_args("unibyte-string-p", &args, 1)?;
    match &args[0] {
        Value::Str(id) => Ok(Value::bool(with_heap(|h| !h.string_is_multibyte(*id)))),
        _ => Ok(Value::Nil),
    }
}

/// `(encode-coding-string STRING CODING-SYSTEM)` -> string
pub(crate) fn builtin_encode_coding_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("encode-coding-string", &args, 2)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("encode-coding-string"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let s = expect_string(&args[0])?;
    let coding = match &args[1] {
        Value::Nil => return Ok(args[0]),
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };
    if !known_coding_system(&coding) {
        return Err(signal("coding-system-error", vec![args[1]]));
    }
    if matches!(
        coding.as_str(),
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac"
    ) {
        return Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
            &storage_string_to_bytes(&s),
        )));
    }
    let bytes = encode_string(&s, &coding);
    Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
        &bytes,
    )))
}

/// `(decode-coding-string STRING CODING-SYSTEM)` -> string
pub(crate) fn builtin_decode_coding_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("decode-coding-string", &args, 2)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("decode-coding-string"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let s = expect_string(&args[0])?;
    let coding = match &args[1] {
        Value::Nil => return Ok(args[0]),
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };
    if !known_coding_system(&coding) {
        return Err(signal("coding-system-error", vec![args[1]]));
    }
    let bytes = storage_string_to_bytes(&s);
    if matches!(
        coding.as_str(),
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac"
    ) {
        return match String::from_utf8(bytes.clone()) {
            Ok(text) => Ok(Value::multibyte_string(text)),
            Err(_) => Ok(Value::multibyte_string(bytes_to_multibyte_raw_string(
                &bytes,
            ))),
        };
    }
    let decoded = decode_bytes(&bytes, &coding);
    if matches!(coding.as_str(), "latin-1" | "iso-8859-1" | "iso-latin-1") {
        let runs = charset_property_runs(&decoded, "iso-8859-1");
        if !runs.is_empty() {
            return Ok(Value::multibyte_string_with_text_properties(decoded, runs));
        }
    }
    Ok(Value::multibyte_string(decoded))
}

/// `(char-or-string-p OBJ)` -> t or nil
pub(crate) fn builtin_char_or_string_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-or-string-p", &args, 1)?;
    let is_char_or_string = match &args[0] {
        Value::Char(_) | Value::Str(_) => true,
        Value::Int(n) => (0..=MAX_CHAR_CODE).contains(n),
        _ => false,
    };
    Ok(Value::bool(is_char_or_string))
}

/// `(char-displayable-p CHAR)` -> t, nil, or `unicode`
pub(crate) fn builtin_char_displayable_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-displayable-p", &args, 1)?;
    let code = match &args[0] {
        Value::Char(c) => *c as i64,
        Value::Int(n) => *n,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), *other],
            ));
        }
    };
    if !(0..=MAX_CHAR_CODE).contains(&code) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::Int(code)],
        ));
    }
    if code <= 0x7F {
        return Ok(Value::True);
    }
    if code <= 0x10_FFFF {
        return Ok(Value::symbol("unicode"));
    }
    Ok(Value::Nil)
}

/// `(max-char)` -> integer
pub(crate) fn builtin_max_char(args: Vec<Value>) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("max-char"), Value::Int(args.len() as i64)],
        ));
    }
    let unicode_only = args.first().is_some_and(|v| !v.is_nil());
    Ok(Value::Int(if unicode_only {
        0x10_FFFF
    } else {
        MAX_CHAR_CODE
    }))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::error::Flow;
    use crate::emacs_core::value::get_string_text_properties;

    #[test]
    fn ascii_width() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width(' '), 1);
        assert_eq!(char_width('Z'), 1);
    }

    #[test]
    fn cjk_width() {
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('日'), 2);
        assert_eq!(char_width('あ'), 2);
        assert_eq!(char_width('ア'), 2);
    }

    #[test]
    fn control_char_width() {
        assert_eq!(char_width('\0'), 2);
        assert_eq!(char_width('\x01'), 2); // ^A
        assert_eq!(char_width('\n'), 0);
        assert_eq!(char_width('\x7f'), 2); // ^?
        assert_eq!(char_width('\u{0080}'), 4);
        assert_eq!(char_width('\u{009f}'), 4);
    }

    #[test]
    fn string_width_mixed() {
        assert_eq!(string_width("hello"), 5);
        assert_eq!(string_width("中文"), 4);
        assert_eq!(string_width("hi中"), 4);
    }

    #[test]
    fn builtin_string_bytes_counts_utf8_length() {
        let result = builtin_string_bytes(vec![Value::string("Aé中")]).unwrap();
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn builtin_char_displayable_p_matches_oracle_bounds_and_types() {
        assert_eq!(
            builtin_char_displayable_p(vec![Value::Int('a' as i64)]).unwrap(),
            Value::True
        );
        assert_eq!(
            builtin_char_displayable_p(vec![Value::Int(0x00E9)]).unwrap(),
            Value::symbol("unicode")
        );
        assert_eq!(
            builtin_char_displayable_p(vec![Value::Int(0x11_0000)]).unwrap(),
            Value::Nil
        );

        let overflow = builtin_char_displayable_p(vec![Value::Int(0x40_0000)])
            .expect_err("overflow char code should signal wrong-type-argument characterp");
        match overflow {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let non_number = builtin_char_displayable_p(vec![Value::symbol("x")])
            .expect_err("non-number should signal number-or-marker-p");
        match non_number {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("number-or-marker-p"), Value::symbol("x")]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_char_width_matches_oracle_control_and_bounds() {
        assert_eq!(
            builtin_char_width(vec![Value::Int(0)]).unwrap(),
            Value::Int(2)
        );
        assert_eq!(
            builtin_char_width(vec![Value::Int(9)]).unwrap(),
            Value::Int(8)
        );
        assert_eq!(
            builtin_char_width(vec![Value::Int(10)]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            builtin_char_width(vec![Value::Int(0x80)]).unwrap(),
            Value::Int(4)
        );
        assert_eq!(
            builtin_char_width(vec![Value::Int(0x11_0000)]).unwrap(),
            Value::Int(1)
        );

        let negative = builtin_char_width(vec![Value::Int(-1)])
            .expect_err("negative character code should signal");
        match negative {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("characterp"), Value::Int(-1)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let overflow = builtin_char_width(vec![Value::Int(0x40_0000)])
            .expect_err("overflow character code should signal");
        match overflow {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::Int(0x40_0000)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_char_or_string_p_respects_character_bounds() {
        assert_eq!(
            builtin_char_or_string_p(vec![Value::Int(0)]).unwrap(),
            Value::True
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::Int(0x3F_FFFF)]).unwrap(),
            Value::True
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::Int(-1)]).unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::Int(0x40_0000)]).unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::symbol("x")]).unwrap(),
            Value::Nil
        );
    }

    #[test]
    fn builtin_max_char_optional_unicode_matches_oracle() {
        assert_eq!(builtin_max_char(vec![]).unwrap(), Value::Int(0x3F_FFFF));
        assert_eq!(
            builtin_max_char(vec![Value::Nil]).unwrap(),
            Value::Int(0x3F_FFFF)
        );
        assert_eq!(
            builtin_max_char(vec![Value::True]).unwrap(),
            Value::Int(0x10_FFFF)
        );
        assert_eq!(
            builtin_max_char(vec![Value::symbol("foo")]).unwrap(),
            Value::Int(0x10_FFFF)
        );

        let wrong_arity = builtin_max_char(vec![Value::Int(1), Value::Int(2)])
            .expect_err("max-char should reject more than one argument");
        match wrong_arity {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(sig.data, vec![Value::symbol("max-char"), Value::Int(2)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_coding_string_helpers_enforce_max_arity() {
        let encode_over = builtin_encode_coding_string(vec![
            Value::string("a"),
            Value::symbol("utf-8"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ])
        .expect_err("encode-coding-string should reject more than four arguments");
        match encode_over {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("encode-coding-string"), Value::Int(5)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let decode_over = builtin_decode_coding_string(vec![
            Value::string("a"),
            Value::symbol("utf-8"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ])
        .expect_err("decode-coding-string should reject more than four arguments");
        match decode_over {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("decode-coding-string"), Value::Int(5)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_coding_string_helpers_runtime_match_oracle_core_cases() {
        use crate::emacs_core::string_escape::decode_storage_char_codes;

        let encoded =
            builtin_encode_coding_string(vec![Value::string("é"), Value::symbol("utf-8")])
                .expect("encode-coding-string should evaluate");
        let encoded_text = encoded
            .as_str()
            .expect("encode-coding-string should return a string");
        assert_eq!(decode_storage_char_codes(encoded_text), vec![0xC3, 0xA9]);

        let decode_utf8 =
            builtin_decode_coding_string(vec![Value::string("é"), Value::symbol("utf-8")])
                .expect("decode-coding-string should evaluate");
        assert_eq!(decode_utf8, Value::string("é"));

        let nil_encode =
            builtin_encode_coding_string(vec![Value::string("é"), Value::Nil]).expect("nil coding");
        assert_eq!(nil_encode, Value::string("é"));

        let nil_decode =
            builtin_decode_coding_string(vec![Value::string("é"), Value::Nil]).expect("nil coding");
        assert_eq!(nil_decode, Value::string("é"));

        let coding_string =
            builtin_encode_coding_string(vec![Value::string("a"), Value::string("utf-8")])
                .expect_err("string coding-system should signal symbolp");
        match coding_string {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("symbolp"), Value::string("utf-8")]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let unknown_encode = builtin_encode_coding_string(vec![
            Value::string("a"),
            Value::symbol("vm-no-such-coding"),
        ])
        .expect_err("unknown coding-system should signal coding-system-error");
        match unknown_encode {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "coding-system-error");
                assert_eq!(sig.data, vec![Value::symbol("vm-no-such-coding")]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let unknown_decode = builtin_decode_coding_string(vec![
            Value::string("a"),
            Value::symbol("vm-no-such-coding"),
        ])
        .expect_err("unknown coding-system should signal coding-system-error");
        match unknown_decode {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "coding-system-error");
                assert_eq!(sig.data, vec![Value::symbol("vm-no-such-coding")]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let unibyte = bytes_to_unibyte_storage_string(&[0xE9]);
        let decoded_unibyte = builtin_decode_coding_string(vec![
            Value::string(unibyte.clone()),
            Value::symbol("utf-8"),
        ])
        .expect("decode-coding-string should preserve invalid bytes");
        let decoded_unibyte_text = decoded_unibyte
            .as_str()
            .expect("decode-coding-string should return string");
        assert_eq!(
            decode_storage_char_codes(decoded_unibyte_text),
            vec![0x3FFF00 + 0xE9]
        );

        let encoded_unibyte =
            builtin_encode_coding_string(vec![Value::string(unibyte), Value::symbol("utf-8")])
                .expect("encode-coding-string should preserve unibyte bytes");
        let encoded_unibyte_text = encoded_unibyte
            .as_str()
            .expect("encode-coding-string should return string");
        assert_eq!(decode_storage_char_codes(encoded_unibyte_text), vec![0xE9]);
    }

    #[test]
    fn decode_latin1_attaches_charset_text_property() {
        let encoded = Value::unibyte_string(bytes_to_unibyte_storage_string(&[0xE9]));
        let decoded = builtin_decode_coding_string(vec![encoded, Value::symbol("latin-1")])
            .expect("latin-1 decode should succeed");
        let Value::Str(id) = decoded else {
            panic!("decode-coding-string should return a string");
        };
        let props = get_string_text_properties(id).expect("decoded string should be propertized");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].start, 0);
        assert_eq!(props[0].end, 1);
        assert_eq!(
            props[0].plist,
            Value::list(vec![Value::symbol("charset"), Value::symbol("iso-8859-1")])
        );
    }

    #[test]
    fn char_byte_conversion() {
        let s = "hello中文";
        assert_eq!(char_to_byte_pos(s, 5), 5);
        assert_eq!(char_to_byte_pos(s, 6), 8); // '中' is 3 bytes
        assert_eq!(byte_to_char_pos(s, 5), 5);
        assert_eq!(byte_to_char_pos(s, 8), 6);
    }

    #[test]
    fn encoding_utf8() {
        let bytes = encode_string("hello", "utf-8");
        assert_eq!(bytes, b"hello");
        let decoded = decode_bytes(b"hello", "utf-8");
        assert_eq!(decoded, "hello");
    }

    #[test]
    fn encoding_latin1() {
        let bytes = encode_string("café", "latin-1");
        assert_eq!(bytes.len(), 4); // é maps to 0xe9
        let decoded = decode_bytes(&[0x63, 0x61, 0x66, 0xe9], "latin-1");
        assert_eq!(decoded, "café");
    }

    #[test]
    fn glyphless_display() {
        assert_eq!(glyphless_char_display('\x01'), "^A");
        assert_eq!(glyphless_char_display('\x7f'), "^?");
        assert_eq!(glyphless_char_display('\u{FEFF}'), "\\uFEFF");
    }

    #[test]
    fn multibyte_detection() {
        assert!(!is_multibyte_string("hello"));
        assert!(is_multibyte_string("héllo"));
        assert!(is_multibyte_string("中文"));
    }

    #[test]
    fn multibyte_detection_treats_unibyte_storage_as_unibyte() {
        let unibyte_ascii =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(b"abc");
        assert!(!is_multibyte_string(&unibyte_ascii));

        let unibyte_utf8 =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0xC3, 0xA9]);
        assert!(!is_multibyte_string(&unibyte_utf8));
    }

    #[test]
    fn builtin_multibyte_string_p_matches_oracle_non_string_and_unibyte_storage() {
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string("abc")]).unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string("é")]).unwrap(),
            Value::True
        );

        let unibyte_ascii =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(b"abc");
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string(unibyte_ascii)]).unwrap(),
            Value::Nil
        );

        assert_eq!(
            builtin_multibyte_string_p(vec![Value::Int(1)]).unwrap(),
            Value::Nil
        );
    }

    #[test]
    fn builtin_unibyte_string_p_basics() {
        assert_eq!(
            builtin_unibyte_string_p(vec![Value::string("hello")]).unwrap(),
            Value::True
        );
        assert_eq!(
            builtin_unibyte_string_p(vec![Value::string("héllo")]).unwrap(),
            Value::Nil
        );
    }

    #[test]
    fn builtin_unibyte_string_p_errors() {
        assert!(builtin_unibyte_string_p(vec![]).is_err());
        assert!(builtin_unibyte_string_p(vec![Value::Int(1)]).is_err());
    }

    #[test]
    fn printable_check() {
        assert!(is_printable('a'));
        assert!(is_printable('中'));
        assert!(!is_printable('\x00'));
        assert!(!is_printable('\x7f'));
    }
}
