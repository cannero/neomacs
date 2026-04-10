//! Character encoding, multibyte support, and character utilities.
//!
//! Neomacs uses UTF-8 internally.  This module provides Emacs-compatible
//! character classification, width calculation, and encoding conversion
//! APIs.

use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::string_escape::{
    bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage,
};
use crate::emacs_core::value::{StringTextPropertyRun, Value, ValueKind};

const MAX_CHAR_CODE: i64 = 0x3F_FFFF;
const RAW_BYTE_SENTINEL_BASE: u32 = 0xE000;
const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_BASE: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;

// GNU Emacs seeds the default `char-width-table` from
// `lisp/international/characters.el`.  Keep the built-in fallback aligned with
// that default table rather than maintaining an ad hoc set of wide ranges.
const GNU_DEFAULT_WIDE_RANGES: &[(u32, u32)] = &[
    (0x1100, 0x115F),
    (0x231A, 0x231B),
    (0x2329, 0x232A),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2630, 0x2637),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x268A, 0x268F),
    (0x2690, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x2E80, 0x2E99),
    (0x2E9B, 0x2EF3),
    (0x2F00, 0x2FD5),
    (0x2FF0, 0x2FFF),
    (0x3000, 0x303E),
    (0x3041, 0x3096),
    (0x3099, 0x30FF),
    (0x3105, 0x312F),
    (0x3131, 0x31E5),
    (0x31EF, 0x31EF),
    (0x31F0, 0x3247),
    (0x3250, 0x4DBF),
    (0x4DC0, 0x4DFF),
    (0x4E00, 0xA48C),
    (0xA490, 0xA4C6),
    (0xA960, 0xA97C),
    (0xAC00, 0xD7A3),
    (0xF900, 0xFAFF),
    (0xFE10, 0xFE19),
    (0xFE30, 0xFE6B),
    (0xFF01, 0xFF60),
    (0xFFE0, 0xFFE6),
    (0x16FE0, 0x16FE4),
    (0x16FF0, 0x16FF6),
    (0x17000, 0x187F7),
    (0x18800, 0x18AFF),
    (0x18B00, 0x18CD5),
    (0x18CFF, 0x18CFF),
    (0x18D00, 0x18D1E),
    (0x18D80, 0x18DF2),
    (0x1AFF0, 0x1AFF3),
    (0x1AFF5, 0x1AFFB),
    (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122),
    (0x1B132, 0x1B132),
    (0x1B150, 0x1B152),
    (0x1B155, 0x1B155),
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB),
    (0x1D300, 0x1D356),
    (0x1D360, 0x1D376),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1AD, 0x1F1AD),
    (0x1F200, 0x1F202),
    (0x1F210, 0x1F23B),
    (0x1F240, 0x1F248),
    (0x1F250, 0x1F251),
    (0x1F260, 0x1F265),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F3FA),
    (0x1F3FB, 0x1F3FF),
    (0x1F400, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F5FF),
    (0x1F600, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D8),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA00, 0x1FA53),
    (0x1FA60, 0x1FA6D),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA8A),
    (0x1FA8E, 0x1FAC6),
    (0x1FAC8, 0x1FAC8),
    (0x1FACD, 0x1FADC),
    (0x1FADF, 0x1FAEA),
    (0x1FAEF, 0x1FAF8),
    (0x1FB00, 0x1FB92),
    (0x20000, 0x2FFFF),
    (0x30000, 0x3FFFF),
];

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
    GNU_DEFAULT_WIDE_RANGES
        .iter()
        .any(|&(start, end)| (start..=end).contains(&cp))
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

fn encode_eol_text(s: &str, coding_system: &str) -> String {
    if coding_system.ends_with("-dos") {
        let mut out = String::with_capacity(s.len() + s.matches('\n').count());
        for ch in s.chars() {
            if ch == '\n' {
                out.push('\r');
            }
            out.push(ch);
        }
        return out;
    }

    if coding_system.ends_with("-mac") {
        return s.replace('\n', "\r");
    }

    s.to_string()
}

fn decode_eol_text(bytes: &[u8], coding_system: &str) -> Vec<u8> {
    if coding_system.ends_with("-dos") {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        return out;
    }

    if coding_system.ends_with("-mac") {
        return bytes
            .iter()
            .map(|byte| if *byte == b'\r' { b'\n' } else { *byte })
            .collect();
    }

    bytes.to_vec()
}

fn coding_system_family(coding_system: &str) -> &str {
    coding_system
        .strip_suffix("-unix")
        .or_else(|| coding_system.strip_suffix("-dos"))
        .or_else(|| coding_system.strip_suffix("-mac"))
        .unwrap_or(coding_system)
}

fn push_emacs_utf8_decoded_char(out: &mut String, code: u32) {
    if let Some(ch) = char::from_u32(code) {
        out.push(ch);
    } else if let Some(encoded) = encode_nonunicode_char_for_storage(code) {
        out.push_str(&encoded);
    } else {
        out.push('\u{FFFD}');
    }
}

fn decode_utf8_emacs_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;

    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            out.push(b0 as char);
            i += 1;
            continue;
        }

        if (0xC2..=0xDF).contains(&b0) && i + 1 < bytes.len() {
            let b1 = bytes[i + 1];
            if (b1 & 0xC0) == 0x80 {
                let code = (((b0 & 0x1F) as u32) << 6) | ((b1 & 0x3F) as u32);
                push_emacs_utf8_decoded_char(&mut out, code);
                i += 2;
                continue;
            }
        }

        if (0xE0..=0xEF).contains(&b0) && i + 2 < bytes.len() {
            let (b1, b2) = (bytes[i + 1], bytes[i + 2]);
            if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 {
                let code = (((b0 & 0x0F) as u32) << 12)
                    | (((b1 & 0x3F) as u32) << 6)
                    | ((b2 & 0x3F) as u32);
                push_emacs_utf8_decoded_char(&mut out, code);
                i += 3;
                continue;
            }
        }

        if (0xF0..=0xF7).contains(&b0) && i + 3 < bytes.len() {
            let (b1, b2, b3) = (bytes[i + 1], bytes[i + 2], bytes[i + 3]);
            if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 && (b3 & 0xC0) == 0x80 {
                let code = (((b0 & 0x07) as u32) << 18)
                    | (((b1 & 0x3F) as u32) << 12)
                    | (((b2 & 0x3F) as u32) << 6)
                    | ((b3 & 0x3F) as u32);
                push_emacs_utf8_decoded_char(&mut out, code);
                i += 4;
                continue;
            }
        }

        if (0xF8..=0xFB).contains(&b0) && i + 4 < bytes.len() {
            let (b1, b2, b3, b4) = (bytes[i + 1], bytes[i + 2], bytes[i + 3], bytes[i + 4]);
            if (b1 & 0xC0) == 0x80
                && (b2 & 0xC0) == 0x80
                && (b3 & 0xC0) == 0x80
                && (b4 & 0xC0) == 0x80
            {
                let code = (((b0 & 0x03) as u32) << 24)
                    | (((b1 & 0x3F) as u32) << 18)
                    | (((b2 & 0x3F) as u32) << 12)
                    | (((b3 & 0x3F) as u32) << 6)
                    | ((b4 & 0x3F) as u32);
                push_emacs_utf8_decoded_char(&mut out, code);
                i += 5;
                continue;
            }
        }

        if (0xFC..=0xFD).contains(&b0) && i + 5 < bytes.len() {
            let (b1, b2, b3, b4, b5) = (
                bytes[i + 1],
                bytes[i + 2],
                bytes[i + 3],
                bytes[i + 4],
                bytes[i + 5],
            );
            if (b1 & 0xC0) == 0x80
                && (b2 & 0xC0) == 0x80
                && (b3 & 0xC0) == 0x80
                && (b4 & 0xC0) == 0x80
                && (b5 & 0xC0) == 0x80
            {
                let code = (((b0 & 0x01) as u32) << 30)
                    | (((b1 & 0x3F) as u32) << 24)
                    | (((b2 & 0x3F) as u32) << 18)
                    | (((b3 & 0x3F) as u32) << 12)
                    | (((b4 & 0x3F) as u32) << 6)
                    | ((b5 & 0x3F) as u32);
                push_emacs_utf8_decoded_char(&mut out, code);
                i += 6;
                continue;
            }
        }

        out.push('\u{FFFD}');
        i += 1;
    }

    out
}

fn encode_emacs_utf8_codepoint(code: u32, out: &mut Vec<u8>) {
    if code <= 0x7F {
        out.push(code as u8);
    } else if code <= 0x7FF {
        out.push(0xC0 | ((code >> 6) as u8));
        out.push(0x80 | ((code & 0x3F) as u8));
    } else if code <= 0xFFFF {
        out.push(0xE0 | ((code >> 12) as u8));
        out.push(0x80 | (((code >> 6) & 0x3F) as u8));
        out.push(0x80 | ((code & 0x3F) as u8));
    } else if code <= 0x1F_FFFF {
        out.push(0xF0 | (((code >> 18) & 0x07) as u8));
        out.push(0x80 | (((code >> 12) & 0x3F) as u8));
        out.push(0x80 | (((code >> 6) & 0x3F) as u8));
        out.push(0x80 | ((code & 0x3F) as u8));
    } else if code <= 0x3F_FFFF {
        out.push(0xF8 | (((code >> 24) & 0x03) as u8));
        out.push(0x80 | (((code >> 18) & 0x3F) as u8));
        out.push(0x80 | (((code >> 12) & 0x3F) as u8));
        out.push(0x80 | (((code >> 6) & 0x3F) as u8));
        out.push(0x80 | ((code & 0x3F) as u8));
    } else if code <= 0x7FFF_FFFF {
        out.push(0xFC | (((code >> 30) & 0x01) as u8));
        out.push(0x80 | (((code >> 24) & 0x3F) as u8));
        out.push(0x80 | (((code >> 18) & 0x3F) as u8));
        out.push(0x80 | (((code >> 12) & 0x3F) as u8));
        out.push(0x80 | (((code >> 6) & 0x3F) as u8));
        out.push(0x80 | ((code & 0x3F) as u8));
    }
}

fn encode_utf8_emacs_text(s: &str) -> Vec<u8> {
    // For standard UTF-8 strings (which is what we get from as_str()),
    // the Emacs internal encoding for Unicode chars IS UTF-8, so we
    // just need to iterate the chars and encode them via the Emacs
    // UTF-8 encoder (which handles extended codepoints).
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        encode_emacs_utf8_codepoint(ch as u32, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Encoding conversion
// ---------------------------------------------------------------------------

/// Encode a string to bytes using the specified coding system.
/// Currently only UTF-8 is supported.
pub fn encode_string(s: &str, coding_system: &str) -> Vec<u8> {
    let eol_text = encode_eol_text(s, coding_system);
    match coding_system_family(coding_system) {
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac" => eol_text.as_bytes().to_vec(),
        "utf-8-emacs" => encode_utf8_emacs_text(&eol_text),
        "latin-1" | "iso-8859-1" | "iso-latin-1" => eol_text
            .chars()
            .map(|c| if (c as u32) <= 0xff { c as u8 } else { b'?' })
            .collect(),
        "ascii" | "us-ascii" => eol_text
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect(),
        _ => eol_text.as_bytes().to_vec(), // default to UTF-8
    }
}

/// Decode bytes to a string using the specified coding system.
/// Currently only UTF-8 is supported.
pub fn decode_bytes(bytes: &[u8], coding_system: &str) -> String {
    let bytes = decode_eol_text(bytes, coding_system);
    match coding_system_family(coding_system) {
        "utf-8" => String::from_utf8_lossy(&bytes).into_owned(),
        "utf-8-emacs" => decode_utf8_emacs_bytes(&bytes),
        "latin-1" | "iso-8859-1" | "iso-latin-1" => bytes.iter().map(|&b| b as char).collect(),
        "ascii" | "us-ascii" => bytes
            .iter()
            .map(|&b| if b < 128 { b as char } else { '?' })
            .collect(),
        _ => String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn is_byte_preserving_coding_system(coding_system: &str) -> bool {
    matches!(
        coding_system,
        "binary" | "no-conversion" | "raw-text" | "raw-text-unix" | "raw-text-dos" | "raw-text-mac"
    )
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(val: &Value) -> Result<String, crate::emacs_core::error::Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn known_coding_system(name: &str) -> bool {
    crate::emacs_core::coding::CodingSystemManager::new().is_known(name)
}

/// `(char-width CHAR)` -> integer
pub(crate) fn builtin_char_width(args: Vec<Value>) -> EvalResult {
    expect_args("char-width", &args, 1)?;
    let code = match args[0].kind() {
        ValueKind::Fixnum(c) => c as i64,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            ));
        }
    };
    if !(0..=MAX_CHAR_CODE).contains(&code) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::fixnum(code)],
        ));
    }
    // Non-Unicode char codes still have width 1 in Emacs.
    if code > 0x10_FFFF {
        return Ok(Value::fixnum(1));
    }
    let width = char::from_u32(code as u32).map(char_width).unwrap_or(1);
    Ok(Value::fixnum(width as i64))
}

/// `(string-bytes STRING)` -> integer byte length of STRING.
pub(crate) fn builtin_string_bytes(args: Vec<Value>) -> EvalResult {
    expect_args("string-bytes", &args, 1)?;
    let string = args[0].as_lisp_string().ok_or_else(|| {
        signal("wrong-type-argument", vec![Value::symbol("stringp"), args[0]])
    })?;
    Ok(Value::fixnum(string.sbytes() as i64))
}

/// `(multibyte-string-p STRING)` -> t or nil
pub(crate) fn builtin_multibyte_string_p(args: Vec<Value>) -> EvalResult {
    expect_args("multibyte-string-p", &args, 1)?;
    match args[0].kind() {
        ValueKind::String => Ok(Value::bool_val(args[0].string_is_multibyte())),
        _ => Ok(Value::NIL),
    }
}

/// `(unibyte-string-p STRING)` -> t or nil
#[cfg(test)]
pub(crate) fn builtin_unibyte_string_p(args: Vec<Value>) -> EvalResult {
    expect_args("unibyte-string-p", &args, 1)?;
    match args[0].kind() {
        ValueKind::String => Ok(Value::bool_val(!args[0].string_is_multibyte())),
        _ => Ok(Value::NIL),
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let s = expect_string(&args[0])?;
    let coding = match args[1].kind() {
        ValueKind::Nil => return Ok(args[0]),
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };
    if !known_coding_system(&coding) {
        return Err(signal("coding-system-error", vec![args[1]]));
    }
    if matches!(coding_system_family(&coding), "utf-8" | "utf-8-emacs") {
        let bytes = encode_string(&s, &coding);
        return Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
            &bytes,
        )));
    }
    if is_byte_preserving_coding_system(&coding) {
        let encoded = if coding.starts_with("raw-text") {
            encode_eol_text(&s, &coding)
        } else {
            s.clone()
        };
        return Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
            &storage_string_to_bytes(&encoded),
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
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let s = expect_string(&args[0])?;
    let coding = match args[1].kind() {
        ValueKind::Nil => return Ok(args[0]),
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };
    if !known_coding_system(&coding) {
        return Err(signal("coding-system-error", vec![args[1]]));
    }
    let bytes = storage_string_to_bytes(&s);
    if is_byte_preserving_coding_system(&coding) {
        let bytes = if coding.starts_with("raw-text") {
            decode_eol_text(&bytes, &coding)
        } else {
            bytes
        };
        return Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
            &bytes,
        )));
    }
    if matches!(coding_system_family(&coding), "utf-8" | "utf-8-emacs") {
        let decoded = decode_bytes(&bytes, &coding);
        return Ok(Value::multibyte_string(decoded));
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
    // GNU `Fchar_or_string_p` (`src/data.c`) only accepts fixnums in
    // the valid character code range [0, MAX_CHAR_CODE = 0x3FFFFF].
    // Negative or out-of-range integers must return nil.
    let is_char_or_string = match args[0].kind() {
        ValueKind::Fixnum(n) => (0..=MAX_CHAR_CODE).contains(&n),
        ValueKind::String => true,
        _ => false,
    };
    Ok(Value::bool_val(is_char_or_string))
}

/// `(char-displayable-p CHAR)` -> t, nil, or `unicode`
pub(crate) fn builtin_char_displayable_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-displayable-p", &args, 1)?;
    let code = match args[0].kind() {
        ValueKind::Fixnum(c) => c as i64,
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("number-or-marker-p"), args[0]],
            ));
        }
    };
    if !(0..=MAX_CHAR_CODE).contains(&code) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), Value::fixnum(code)],
        ));
    }
    if code <= 0x7F {
        return Ok(Value::T);
    }
    if code <= 0x10_FFFF {
        return Ok(Value::symbol("unicode"));
    }
    Ok(Value::NIL)
}

/// `(max-char)` -> integer
pub(crate) fn builtin_max_char(args: Vec<Value>) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("max-char"), Value::fixnum(args.len() as i64)],
        ));
    }
    let unicode_only = args.first().is_some_and(|v| !v.is_nil());
    Ok(Value::fixnum(if unicode_only {
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
    use crate::emacs_core::value::get_string_text_properties_for_value;

    #[test]
    fn ascii_width() {
        crate::test_utils::init_test_tracing();
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width(' '), 1);
        assert_eq!(char_width('Z'), 1);
    }

    #[test]
    fn cjk_width() {
        crate::test_utils::init_test_tracing();
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('日'), 2);
        assert_eq!(char_width('あ'), 2);
        assert_eq!(char_width('ア'), 2);
    }

    #[test]
    fn gnu_default_emoji_symbol_widths() {
        crate::test_utils::init_test_tracing();
        assert_eq!(char_width('\u{2603}'), 1);
        assert_eq!(char_width('\u{2615}'), 2);
        assert_eq!(char_width('\u{263A}'), 1);
    }

    #[test]
    fn control_char_width() {
        crate::test_utils::init_test_tracing();
        assert_eq!(char_width('\0'), 2);
        assert_eq!(char_width('\x01'), 2); // ^A
        assert_eq!(char_width('\n'), 0);
        assert_eq!(char_width('\x7f'), 2); // ^?
        assert_eq!(char_width('\u{0080}'), 4);
        assert_eq!(char_width('\u{009f}'), 4);
    }

    #[test]
    fn string_width_mixed() {
        crate::test_utils::init_test_tracing();
        assert_eq!(string_width("hello"), 5);
        assert_eq!(string_width("中文"), 4);
        assert_eq!(string_width("hi中"), 4);
    }

    #[test]
    fn builtin_string_bytes_counts_utf8_length() {
        crate::test_utils::init_test_tracing();
        let result = builtin_string_bytes(vec![Value::string("Aé中")]).unwrap();
        assert_eq!(result, Value::fixnum(6));
    }

    #[test]
    fn builtin_char_displayable_p_matches_oracle_bounds_and_types() {
        crate::test_utils::init_test_tracing();
        assert_eq!(
            builtin_char_displayable_p(vec![Value::fixnum('a' as i64)]).unwrap(),
            Value::T
        );
        assert_eq!(
            builtin_char_displayable_p(vec![Value::fixnum(0x00E9)]).unwrap(),
            Value::symbol("unicode")
        );
        assert_eq!(
            builtin_char_displayable_p(vec![Value::fixnum(0x11_0000)]).unwrap(),
            Value::NIL
        );

        let overflow = builtin_char_displayable_p(vec![Value::fixnum(0x40_0000)])
            .expect_err("overflow char code should signal wrong-type-argument characterp");
        match overflow {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::fixnum(0x40_0000)]
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
        crate::test_utils::init_test_tracing();
        assert_eq!(
            builtin_char_width(vec![Value::fixnum(0)]).unwrap(),
            Value::fixnum(2)
        );
        assert_eq!(
            builtin_char_width(vec![Value::fixnum(9)]).unwrap(),
            Value::fixnum(8)
        );
        assert_eq!(
            builtin_char_width(vec![Value::fixnum(10)]).unwrap(),
            Value::fixnum(0)
        );
        assert_eq!(
            builtin_char_width(vec![Value::fixnum(0x80)]).unwrap(),
            Value::fixnum(4)
        );
        assert_eq!(
            builtin_char_width(vec![Value::fixnum(0x11_0000)]).unwrap(),
            Value::fixnum(1)
        );

        let negative = builtin_char_width(vec![Value::fixnum(-1)])
            .expect_err("negative character code should signal");
        match negative {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::fixnum(-1)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let overflow = builtin_char_width(vec![Value::fixnum(0x40_0000)])
            .expect_err("overflow character code should signal");
        match overflow {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("characterp"), Value::fixnum(0x40_0000)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_char_or_string_p_respects_character_bounds() {
        crate::test_utils::init_test_tracing();
        assert_eq!(
            builtin_char_or_string_p(vec![Value::fixnum(0)]).unwrap(),
            Value::T
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::fixnum(0x3F_FFFF)]).unwrap(),
            Value::T
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::fixnum(-1)]).unwrap(),
            Value::NIL
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::fixnum(0x40_0000)]).unwrap(),
            Value::NIL
        );
        assert_eq!(
            builtin_char_or_string_p(vec![Value::symbol("x")]).unwrap(),
            Value::NIL
        );
    }

    #[test]
    fn builtin_max_char_optional_unicode_matches_oracle() {
        crate::test_utils::init_test_tracing();
        assert_eq!(builtin_max_char(vec![]).unwrap(), Value::fixnum(0x3F_FFFF));
        assert_eq!(
            builtin_max_char(vec![Value::NIL]).unwrap(),
            Value::fixnum(0x3F_FFFF)
        );
        assert_eq!(
            builtin_max_char(vec![Value::T]).unwrap(),
            Value::fixnum(0x10_FFFF)
        );
        assert_eq!(
            builtin_max_char(vec![Value::symbol("foo")]).unwrap(),
            Value::fixnum(0x10_FFFF)
        );

        let wrong_arity = builtin_max_char(vec![Value::fixnum(1), Value::fixnum(2)])
            .expect_err("max-char should reject more than one argument");
        match wrong_arity {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(sig.data, vec![Value::symbol("max-char"), Value::fixnum(2)]);
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_coding_string_helpers_enforce_max_arity() {
        crate::test_utils::init_test_tracing();
        let encode_over = builtin_encode_coding_string(vec![
            Value::string("a"),
            Value::symbol("utf-8"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ])
        .expect_err("encode-coding-string should reject more than four arguments");
        match encode_over {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("encode-coding-string"), Value::fixnum(5)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }

        let decode_over = builtin_decode_coding_string(vec![
            Value::string("a"),
            Value::symbol("utf-8"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ])
        .expect_err("decode-coding-string should reject more than four arguments");
        match decode_over {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("decode-coding-string"), Value::fixnum(5)]
                );
            }
            other => panic!("expected signal, got: {other:?}"),
        }
    }

    #[test]
    fn builtin_coding_string_helpers_runtime_match_oracle_core_cases() {
        crate::test_utils::init_test_tracing();
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
            builtin_encode_coding_string(vec![Value::string("é"), Value::NIL]).expect("nil coding");
        assert_eq!(nil_encode, Value::string("é"));

        let nil_decode =
            builtin_decode_coding_string(vec![Value::string("é"), Value::NIL]).expect("nil coding");
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
    fn builtin_coding_string_helpers_accept_iso_8859_15_alias() {
        crate::test_utils::init_test_tracing();
        let encoded =
            builtin_encode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-15")])
                .expect("iso-8859-15 should be accepted as a known coding system");
        assert_eq!(encoded.as_str(), Some("abc"));

        let decoded =
            builtin_decode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-15")])
                .expect("iso-8859-15 should be accepted as a known coding system");
        assert_eq!(decoded.as_str(), Some("abc"));
    }

    #[test]
    fn builtin_coding_string_helpers_accept_iso_8859_9_alias() {
        crate::test_utils::init_test_tracing();
        let encoded =
            builtin_encode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-9")])
                .expect("iso-8859-9 should be accepted as a known coding system");
        assert_eq!(encoded.as_str(), Some("abc"));

        let decoded =
            builtin_decode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-9")])
                .expect("iso-8859-9 should be accepted as a known coding system");
        assert_eq!(decoded.as_str(), Some("abc"));
    }

    #[test]
    fn decode_latin1_attaches_charset_text_property() {
        crate::test_utils::init_test_tracing();
        let encoded = Value::unibyte_string(bytes_to_unibyte_storage_string(&[0xE9]));
        let decoded = builtin_decode_coding_string(vec![encoded, Value::symbol("latin-1")])
            .expect("latin-1 decode should succeed");
        if !decoded.is_string() {
            panic!("decode-coding-string should return a string");
        };
        let props = get_string_text_properties_for_value(decoded)
            .expect("decoded string should be propertized");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].start, 0);
        assert_eq!(props[0].end, 1);
        assert_eq!(
            props[0].plist,
            Value::list(vec![Value::symbol("charset"), Value::symbol("iso-8859-1")])
        );
    }

    #[test]
    fn encode_no_conversion_preserves_unibyte_storage_bytes() {
        crate::test_utils::init_test_tracing();
        let source = Value::unibyte_string(bytes_to_unibyte_storage_string(&[0xE9]));
        let encoded =
            builtin_encode_coding_string(vec![source, Value::symbol("no-conversion")]).unwrap();
        if !encoded.is_string() {
            panic!("encode-coding-string should return a string");
        };
        assert!(!encoded.string_is_multibyte());
        assert_eq!(
            encoded.as_str().unwrap().to_owned(),
            bytes_to_unibyte_storage_string(&[0xE9])
        );
    }

    #[test]
    fn decode_no_conversion_returns_unibyte_bytes_for_non_ascii_input() {
        crate::test_utils::init_test_tracing();
        let encoded =
            builtin_encode_coding_string(vec![Value::string("é"), Value::symbol("no-conversion")])
                .expect("encoding should succeed");
        let decoded =
            builtin_decode_coding_string(vec![encoded, Value::symbol("no-conversion")]).unwrap();
        if !decoded.is_string() {
            panic!("decode-coding-string should return a string");
        };
        assert!(!decoded.string_is_multibyte());
        assert_eq!(
            decoded.as_str().unwrap().to_owned(),
            bytes_to_unibyte_storage_string(&[0xC3, 0xA9])
        );
    }

    #[test]
    fn char_byte_conversion() {
        crate::test_utils::init_test_tracing();
        let s = "hello中文";
        assert_eq!(char_to_byte_pos(s, 5), 5);
        assert_eq!(char_to_byte_pos(s, 6), 8); // '中' is 3 bytes
        assert_eq!(byte_to_char_pos(s, 5), 5);
        assert_eq!(byte_to_char_pos(s, 8), 6);
    }

    #[test]
    fn encoding_utf8() {
        crate::test_utils::init_test_tracing();
        let bytes = encode_string("hello", "utf-8");
        assert_eq!(bytes, b"hello");
        let decoded = decode_bytes(b"hello", "utf-8");
        assert_eq!(decoded, "hello");
    }

    #[test]
    fn encoding_utf8_dos_applies_eol_conversion() {
        crate::test_utils::init_test_tracing();
        let bytes = encode_string("a\nb", "utf-8-dos");
        assert_eq!(bytes, b"a\r\nb");
        let decoded = decode_bytes(b"a\r\nb", "utf-8-dos");
        assert_eq!(decoded, "a\nb");
    }

    #[test]
    fn raw_text_dos_preserves_bytes_but_converts_eol() {
        crate::test_utils::init_test_tracing();
        let encoded = builtin_encode_coding_string(vec![
            Value::string("a\nb"),
            Value::symbol("raw-text-dos"),
        ])
        .unwrap();
        if !encoded.is_string() {
            panic!("encode-coding-string should return a string");
        };
        assert_eq!(
            encoded.as_str().unwrap().to_owned(),
            bytes_to_unibyte_storage_string(b"a\r\nb")
        );

        let decoded = builtin_decode_coding_string(vec![
            Value::unibyte_string(bytes_to_unibyte_storage_string(b"a\r\nb")),
            Value::symbol("raw-text-dos"),
        ])
        .unwrap();
        if !decoded.is_string() {
            panic!("decode-coding-string should return a string");
        };
        assert_eq!(
            decoded.as_str().unwrap().to_owned(),
            bytes_to_unibyte_storage_string(b"a\nb")
        );
    }

    #[test]
    fn encoding_latin1() {
        crate::test_utils::init_test_tracing();
        let bytes = encode_string("café", "latin-1");
        assert_eq!(bytes.len(), 4); // é maps to 0xe9
        let decoded = decode_bytes(&[0x63, 0x61, 0x66, 0xe9], "latin-1");
        assert_eq!(decoded, "café");
    }

    #[test]
    fn glyphless_display() {
        crate::test_utils::init_test_tracing();
        assert_eq!(glyphless_char_display('\x01'), "^A");
        assert_eq!(glyphless_char_display('\x7f'), "^?");
        assert_eq!(glyphless_char_display('\u{FEFF}'), "\\uFEFF");
    }

    #[test]
    fn multibyte_detection() {
        crate::test_utils::init_test_tracing();
        assert!(!is_multibyte_string("hello"));
        assert!(is_multibyte_string("héllo"));
        assert!(is_multibyte_string("中文"));
    }

    #[test]
    fn multibyte_detection_treats_unibyte_storage_as_unibyte() {
        crate::test_utils::init_test_tracing();
        let unibyte_ascii =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(b"abc");
        assert!(!is_multibyte_string(&unibyte_ascii));

        let unibyte_utf8 =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0xC3, 0xA9]);
        assert!(!is_multibyte_string(&unibyte_utf8));
    }

    #[test]
    fn builtin_multibyte_string_p_matches_oracle_non_string_and_unibyte_storage() {
        crate::test_utils::init_test_tracing();
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string("abc")]).unwrap(),
            Value::NIL
        );
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string("é")]).unwrap(),
            Value::T
        );

        let unibyte_ascii =
            crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(b"abc");
        assert_eq!(
            builtin_multibyte_string_p(vec![Value::string(unibyte_ascii)]).unwrap(),
            Value::NIL
        );

        assert_eq!(
            builtin_multibyte_string_p(vec![Value::fixnum(1)]).unwrap(),
            Value::NIL
        );
    }

    #[test]
    fn builtin_unibyte_string_p_basics() {
        crate::test_utils::init_test_tracing();
        assert_eq!(
            builtin_unibyte_string_p(vec![Value::string("hello")]).unwrap(),
            Value::T
        );
        assert_eq!(
            builtin_unibyte_string_p(vec![Value::string("héllo")]).unwrap(),
            Value::NIL
        );
    }

    #[test]
    fn builtin_unibyte_string_p_errors() {
        crate::test_utils::init_test_tracing();
        // Wrong arity signals error.
        assert!(builtin_unibyte_string_p(vec![]).is_err());
        // Non-string arg returns nil (type predicates don't error on wrong type).
        assert_eq!(
            builtin_unibyte_string_p(vec![Value::fixnum(1)]).unwrap(),
            Value::NIL
        );
    }

    #[test]
    fn printable_check() {
        crate::test_utils::init_test_tracing();
        assert!(is_printable('a'));
        assert!(is_printable('中'));
        assert!(!is_printable('\x00'));
        assert!(!is_printable('\x7f'));
    }
}
