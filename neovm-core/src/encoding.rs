//! Character encoding, multibyte support, and character utilities.
//!
//! Neomacs uses UTF-8 internally.  This module provides Emacs-compatible
//! character classification, width calculation, and encoding conversion
//! APIs.

use crate::emacs_core::intern::resolve_sym;
// encoding.rs: sentinel imports removed; using emacs_char + LispString directly
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

const ZERO_WIDTH_RANGES: &[(u32, u32)] = &[
    (0x0300, 0x036F),
    (0x0483, 0x0489),
    (0x0591, 0x05BD),
    (0x0610, 0x061A),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC),
    (0x0730, 0x074A),
    (0x0900, 0x0903),
    (0x093A, 0x094F),
    (0x0E31, 0x0E3A),
    (0x0E47, 0x0E4E),
    (0x1160, 0x11FF),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF),
    (0x1D167, 0x1D169),
    (0x1D173, 0x1D182),
    (0xE0020, 0xE007F),
    (0xE0100, 0xE01EF),
];

#[inline]
fn codepoint_in_sorted_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    let mut low = 0usize;
    let mut high = ranges.len();
    while low < high {
        let mid = (low + high) / 2;
        let (start, end) = ranges[mid];
        if cp < start {
            high = mid;
        } else if cp > end {
            low = mid + 1;
        } else {
            return true;
        }
    }
    false
}

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
    codepoint_in_sorted_ranges(c as u32, ZERO_WIDTH_RANGES)
}

/// Whether the character is full-width (East Asian wide).
fn is_wide_char(c: char) -> bool {
    codepoint_in_sorted_ranges(c as u32, GNU_DEFAULT_WIDE_RANGES)
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

fn encode_eol_bytes(bytes: &[u8], coding_system: &str) -> Vec<u8> {
    if coding_system.ends_with("-dos") {
        let mut out =
            Vec::with_capacity(bytes.len() + bytes.iter().filter(|&&byte| byte == b'\n').count());
        for &byte in bytes {
            if byte == b'\n' {
                out.push(b'\r');
            }
            out.push(byte);
        }
        return out;
    }

    if coding_system.ends_with("-mac") {
        return bytes
            .iter()
            .map(|&byte| if byte == b'\n' { b'\r' } else { byte })
            .collect();
    }

    bytes.to_vec()
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
    } else if let Some(encoded) =
        crate::emacs_core::string_escape::encode_nonunicode_char_for_storage(code)
    {
        // Non-Unicode Emacs chars (raw bytes, extended range) are encoded
        // using sentinel codepoints in String-based paths (buffer layer,
        // coding system layer).
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

        // Invalid byte: treat as raw-byte char (matching GNU Emacs behavior).
        let byte = bytes[i];
        let code = crate::emacs_core::emacs_char::byte8_to_char(byte);
        push_emacs_utf8_decoded_char(&mut out, code);
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
    // the Emacs internal encoding for Unicode chars IS UTF-8.
    // Sentinel codepoints are translated back to their raw byte values
    // before encoding.
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        // Translate sentinel codepoints back to raw byte values
        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
            let byte = (cp - RAW_BYTE_SENTINEL_BASE) as u8;
            out.push(byte);
        } else if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            let byte = (cp - UNIBYTE_BYTE_SENTINEL_BASE) as u8;
            out.push(byte);
        } else {
            encode_emacs_utf8_codepoint(cp, &mut out);
        }
    }
    out
}

pub fn encode_lisp_string(s: &crate::heap_types::LispString, coding_system: &str) -> Vec<u8> {
    let family = coding_system_family(coding_system);
    if matches!(family, "utf-8" | "utf-8-emacs") || is_byte_preserving_coding_system(coding_system)
    {
        return encode_eol_bytes(s.as_bytes(), coding_system);
    }

    let mut out = Vec::with_capacity(s.sbytes());
    let mut push_encoded = |code: u32| match family {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => {
            if code <= 0xFF {
                out.push(code as u8);
            } else if crate::emacs_core::emacs_char::char_byte8_p(code) {
                out.push(crate::emacs_core::emacs_char::char_to_byte8(code));
            } else {
                out.push(b'?');
            }
        }
        "ascii" | "us-ascii" => {
            if code <= 0x7F {
                out.push(code as u8);
            } else {
                out.push(b'?');
            }
        }
        _ => {}
    };

    if s.is_multibyte() {
        let bytes = s.as_bytes();
        let mut pos = 0usize;
        while pos < bytes.len() {
            let (code, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
            push_encoded(code);
            pos += len;
        }
    } else {
        for &byte in s.as_bytes() {
            push_encoded(byte as u32);
        }
    }

    encode_eol_bytes(&out, coding_system)
}

// ---------------------------------------------------------------------------
// Encoding conversion
// ---------------------------------------------------------------------------

/// Encode a string to bytes using the specified coding system.
/// Currently only UTF-8 is supported.
pub fn encode_string(s: &str, coding_system: &str) -> Vec<u8> {
    let eol_text = encode_eol_text(s, coding_system);
    match coding_system_family(coding_system) {
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac" | "utf-8-emacs" => {
            encode_utf8_emacs_text(&eol_text)
        }
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
        "utf-8" | "utf-8-emacs" => decode_utf8_emacs_bytes(&bytes),
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
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        if b <= 0x7F {
            out.push(b as char);
            continue;
        }
        let code = crate::emacs_core::emacs_char::byte8_to_char(b);
        push_emacs_utf8_decoded_char(&mut out, code);
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
        ValueKind::String => {
            let ls = val.as_lisp_string().unwrap();
            // For unibyte strings, convert each byte to a char using sentinel encoding
            // (backward compat for buffer/coding-system layer).
            if !ls.is_multibyte() {
                return Ok(
                    crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(
                        ls.as_bytes(),
                    ),
                );
            }
            // For multibyte, try UTF-8 first, fall back to lossy conversion
            Ok(ls
                .as_utf8_str()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| crate::emacs_core::emacs_char::to_utf8_lossy(ls.as_bytes())))
        }
        _other => Err(signal(
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
    let width = char_width(char::from_u32(code as u32).expect("code <= 0x10FFFF must be valid"));
    Ok(Value::fixnum(width as i64))
}

/// `(string-bytes STRING)` -> integer byte length of STRING.
pub(crate) fn builtin_string_bytes(args: Vec<Value>) -> EvalResult {
    expect_args("string-bytes", &args, 1)?;
    let string = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
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
    let string = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
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
    let bytes = encode_lisp_string(string, &coding);
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_unibyte(bytes),
    ))
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
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_unibyte(bytes),
        ));
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
#[path = "encoding_test.rs"]
mod tests;
