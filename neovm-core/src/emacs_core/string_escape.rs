//! Shared Lisp string escaping helpers.

use std::iter::Peekable;
use std::str::Chars;

const RAW_BYTE_SENTINEL_BASE: u32 = 0xE000;
const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const RAW_BYTE_CHAR_MIN: u32 = 0x3FFF80;
const RAW_BYTE_CHAR_MAX: u32 = 0x3FFFFF;
const UNIBYTE_BYTE_SENTINEL_BASE: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;

const EXT_SEQ_PREFIX: u32 = 0xE100;
const EXT_SEQ_LEN_BASE: u32 = 0xE110;
const EXT_SEQ_BYTE_BASE: u32 = 0xE200;
const EXT_SEQ_MAX_LEN: u32 = 6;

/// Encode non-Unicode Emacs character codes as NeoVM internal sentinels.
///
/// Returns `None` for Unicode scalar values (which can be stored directly).
pub(crate) fn encode_nonunicode_char_for_storage(code: u32) -> Option<String> {
    if code <= 0x10FFFF {
        return None;
    }

    if (RAW_BYTE_CHAR_MIN..=RAW_BYTE_CHAR_MAX).contains(&code) {
        // Emacs raw-byte chars map 0x3FFF80..0x3FFFFF -> 0x80..0xFF.
        let raw = code - 0x3FFF00;
        let ch = char::from_u32(RAW_BYTE_SENTINEL_BASE + raw).expect("valid raw-byte sentinel");
        return Some(ch.to_string());
    }

    if code <= 0x3FFFFF {
        let bytes = encode_emacs_extended_utf8(code);
        return Some(encode_extended_sequence_for_storage(&bytes));
    }

    None
}

fn encode_emacs_extended_utf8(code: u32) -> Vec<u8> {
    if code <= 0x7F {
        vec![code as u8]
    } else if code <= 0x7FF {
        vec![0xC0 | ((code >> 6) as u8), 0x80 | ((code & 0x3F) as u8)]
    } else if code <= 0xFFFF {
        vec![
            0xE0 | ((code >> 12) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else if code <= 0x1FFFFF {
        vec![
            0xF0 | (((code >> 18) & 0x07) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else if code <= 0x3FFFFFF {
        vec![
            0xF8 | (((code >> 24) & 0x03) as u8),
            0x80 | (((code >> 18) & 0x3F) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else {
        vec![
            0xFC | (((code >> 30) & 0x01) as u8),
            0x80 | (((code >> 24) & 0x3F) as u8),
            0x80 | (((code >> 18) & 0x3F) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    }
}

fn decode_emacs_extended_utf8(bytes: &[u8]) -> Option<u32> {
    match bytes {
        [b0] => Some(*b0 as u32),
        // Emacs raw bytes 0x80..0xFF may appear as overlong C0/C1 sequences.
        [b0, b1] if (*b0 == 0xC0 || *b0 == 0xC1) && (b1 & 0xC0) == 0x80 => {
            Some(0x80 + (((b0 & 0x01) as u32) << 6) + ((b1 & 0x3F) as u32))
        }
        [b0, b1] if (0xC2..=0xDF).contains(b0) && (b1 & 0xC0) == 0x80 => {
            Some((((b0 & 0x1F) as u32) << 6) | ((b1 & 0x3F) as u32))
        }
        [b0, b1, b2] if (b0 & 0xF0) == 0xE0 && (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 => {
            Some((((b0 & 0x0F) as u32) << 12) | (((b1 & 0x3F) as u32) << 6) | ((b2 & 0x3F) as u32))
        }
        [b0, b1, b2, b3]
            if (b0 & 0xF8) == 0xF0
                && (b1 & 0xC0) == 0x80
                && (b2 & 0xC0) == 0x80
                && (b3 & 0xC0) == 0x80 =>
        {
            Some(
                (((b0 & 0x07) as u32) << 18)
                    | (((b1 & 0x3F) as u32) << 12)
                    | (((b2 & 0x3F) as u32) << 6)
                    | ((b3 & 0x3F) as u32),
            )
        }
        [b0, b1, b2, b3, b4]
            if (b0 & 0xFC) == 0xF8
                && (b1 & 0xC0) == 0x80
                && (b2 & 0xC0) == 0x80
                && (b3 & 0xC0) == 0x80
                && (b4 & 0xC0) == 0x80 =>
        {
            Some(
                (((b0 & 0x03) as u32) << 24)
                    | (((b1 & 0x3F) as u32) << 18)
                    | (((b2 & 0x3F) as u32) << 12)
                    | (((b3 & 0x3F) as u32) << 6)
                    | ((b4 & 0x3F) as u32),
            )
        }
        [b0, b1, b2, b3, b4, b5]
            if (b0 & 0xFE) == 0xFC
                && (b1 & 0xC0) == 0x80
                && (b2 & 0xC0) == 0x80
                && (b3 & 0xC0) == 0x80
                && (b4 & 0xC0) == 0x80
                && (b5 & 0xC0) == 0x80 =>
        {
            Some(
                (((b0 & 0x01) as u32) << 30)
                    | (((b1 & 0x3F) as u32) << 24)
                    | (((b2 & 0x3F) as u32) << 18)
                    | (((b3 & 0x3F) as u32) << 12)
                    | (((b4 & 0x3F) as u32) << 6)
                    | ((b5 & 0x3F) as u32),
            )
        }
        _ => None,
    }
}

fn encode_extended_sequence_for_storage(bytes: &[u8]) -> String {
    let mut out = String::new();
    out.push(char::from_u32(EXT_SEQ_PREFIX).expect("valid extended prefix sentinel"));
    let len_char = char::from_u32(EXT_SEQ_LEN_BASE + bytes.len() as u32)
        .expect("valid extended length sentinel");
    out.push(len_char);
    for b in bytes {
        out.push(
            char::from_u32(EXT_SEQ_BYTE_BASE + (*b as u32)).expect("valid extended byte sentinel"),
        );
    }
    out
}

fn decode_extended_sequence_span(s: &str, start: usize) -> Option<(usize, u32)> {
    let mut iter = s[start..].char_indices();
    let (_, prefix) = iter.next()?;
    if prefix as u32 != EXT_SEQ_PREFIX {
        return None;
    }

    let (len_off, len_ch) = iter.next()?;
    let len_code = len_ch as u32;
    if !(EXT_SEQ_LEN_BASE + 1..=EXT_SEQ_LEN_BASE + EXT_SEQ_MAX_LEN).contains(&len_code) {
        return None;
    }
    let len = (len_code - EXT_SEQ_LEN_BASE) as usize;

    let mut bytes = Vec::with_capacity(len);
    let mut end_rel = len_off + len_ch.len_utf8();
    for _ in 0..len {
        let (byte_off, byte_ch) = iter.next()?;
        let byte_code = byte_ch as u32;
        if !(EXT_SEQ_BYTE_BASE..=EXT_SEQ_BYTE_BASE + 0xFF).contains(&byte_code) {
            return None;
        }
        bytes.push((byte_code - EXT_SEQ_BYTE_BASE) as u8);
        end_rel = byte_off + byte_ch.len_utf8();
    }

    let cp = decode_emacs_extended_utf8(&bytes)?;
    Some((start + end_rel, cp))
}

fn push_unibyte_literal_byte(out: &mut Vec<u8>, byte: u8) {
    match byte {
        b'"' => out.extend_from_slice(br#"\""#),
        b'\\' => out.extend_from_slice(br#"\\"#),
        b if b >= 0x80 => push_octal_escape(out, b),
        b => out.push(b),
    }
}

fn scan_storage_units(s: &str) -> Vec<(usize, usize, u32, usize, usize)> {
    let mut out = Vec::new();
    let mut idx = 0usize;

    while idx < s.len() {
        let ch = s[idx..].chars().next().expect("valid utf-8 char boundary");
        let code = ch as u32;
        let next = idx + ch.len_utf8();

        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&code) {
            let raw = (code - RAW_BYTE_SENTINEL_BASE) as u8;
            out.push((idx, next, 0x3FFF00 + raw as u32, 4, 2));
            idx = next;
            continue;
        }

        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&code) {
            let byte = (code - UNIBYTE_BYTE_SENTINEL_BASE) as u8;
            out.push((idx, next, byte as u32, 1, 1));
            idx = next;
            continue;
        }

        if code == EXT_SEQ_PREFIX {
            if let Some((end, cp)) = decode_extended_sequence_span(s, idx) {
                let byte_len = ((s[idx..end].chars().nth(1).expect("extended len sentinel") as u32)
                    - EXT_SEQ_LEN_BASE) as usize;
                out.push((idx, end, cp, 1, byte_len));
                idx = end;
                continue;
            }
        }

        let width = crate::encoding::char_width(ch);
        out.push((idx, next, code, width, ch.len_utf8()));
        idx = next;
    }

    out
}

/// Losslessly encode potentially non-UTF-8 bytes into internal string storage.
pub(crate) fn bytes_to_storage_string(bytes: &[u8]) -> String {
    if let Ok(utf8) = String::from_utf8(bytes.to_vec()) {
        return utf8;
    }

    let mut out = String::new();
    let max_chunk = EXT_SEQ_MAX_LEN as usize;
    for chunk in bytes.chunks(max_chunk) {
        out.push_str(&encode_extended_sequence_for_storage(chunk));
    }
    out
}

/// Encode raw byte values as a unibyte storage string.
///
/// This keeps byte-oriented Elisp semantics for operations like `aref`,
/// `string-bytes`, and `secure-hash` binary output.
pub(crate) fn bytes_to_unibyte_storage_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        if *b <= 0x7f {
            out.push(char::from(*b));
        } else {
            out.push(
                char::from_u32(UNIBYTE_BYTE_SENTINEL_BASE + (*b as u32))
                    .expect("valid unibyte-byte sentinel"),
            );
        }
    }
    out
}

pub(crate) fn encode_char_code_for_string_storage(code: u32, multibyte: bool) -> Option<String> {
    if !multibyte {
        return (code <= 0xff).then(|| bytes_to_unibyte_storage_string(&[code as u8]));
    }

    if let Some(ch) = char::from_u32(code) {
        return Some(ch.to_string());
    }
    encode_nonunicode_char_for_storage(code)
}

pub(crate) fn decode_storage_units(s: &str) -> Vec<(u32, usize)> {
    scan_storage_units(s)
        .into_iter()
        .map(|(_, _, cp, width, _)| (cp, width))
        .collect()
}

/// Decode NeoVM string storage into Emacs character codes.
pub(crate) fn decode_storage_char_codes(s: &str) -> Vec<u32> {
    decode_storage_units(s)
        .into_iter()
        .map(|(cp, _)| cp)
        .collect()
}

/// Compute Emacs-like display width for NeoVM string storage.
pub(crate) fn storage_string_display_width(s: &str) -> usize {
    decode_storage_units(s)
        .into_iter()
        .map(|(_, width)| width)
        .sum()
}

/// Count logical Emacs characters in NeoVM string storage.
pub(crate) fn storage_char_len(s: &str) -> usize {
    scan_storage_units(s).len()
}

/// Count Emacs string bytes represented by NeoVM string storage.
pub(crate) fn storage_byte_len(s: &str) -> usize {
    scan_storage_units(s)
        .into_iter()
        .map(|(_, _, _, _, byte_len)| byte_len)
        .sum()
}

/// Convert a 0-based Emacs character index to a byte offset in NeoVM string storage.
/// Clamps to the string length if the index is out of bounds.
pub(crate) fn storage_char_to_byte(s: &str, char_idx: usize) -> usize {
    let units = scan_storage_units(s);
    if char_idx >= units.len() {
        s.len()
    } else {
        units[char_idx].0
    }
}

/// Convert a byte offset in NeoVM string storage to a 0-based Emacs character index.
pub(crate) fn storage_byte_to_char(s: &str, byte_pos: usize) -> usize {
    let units = scan_storage_units(s);
    for (i, unit) in units.iter().enumerate() {
        if byte_pos < unit.1 {
            return i;
        }
    }
    units.len()
}

/// Slice NeoVM string storage by logical Emacs character index.
pub(crate) fn storage_substring(s: &str, from: usize, to: usize) -> Option<String> {
    if from > to {
        return None;
    }

    let units = scan_storage_units(s);
    if to > units.len() {
        return None;
    }

    let start_byte = if from == units.len() {
        s.len()
    } else {
        units[from].0
    };
    let end_byte = if to == units.len() {
        s.len()
    } else {
        units[to].0
    };
    Some(s[start_byte..end_byte].to_string())
}

fn decode_extended_sequence(chars: &mut Peekable<Chars<'_>>) -> Option<Vec<u8>> {
    let len_char = chars.peek().copied()?;
    let len_code = len_char as u32;
    if !(EXT_SEQ_LEN_BASE + 1..=EXT_SEQ_LEN_BASE + EXT_SEQ_MAX_LEN).contains(&len_code) {
        return None;
    }
    chars.next();
    let len = (len_code - EXT_SEQ_LEN_BASE) as usize;

    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let b_char = chars.peek().copied()?;
        let b_code = b_char as u32;
        if !(EXT_SEQ_BYTE_BASE..=EXT_SEQ_BYTE_BASE + 0xFF).contains(&b_code) {
            return None;
        }
        chars.next();
        out.push((b_code - EXT_SEQ_BYTE_BASE) as u8);
    }
    Some(out)
}

fn push_octal_escape(out: &mut Vec<u8>, byte: u8) {
    out.push(b'\\');
    out.extend_from_slice(format!("{:03o}", byte).as_bytes());
}

fn push_escaped_literal_byte(out: &mut Vec<u8>, byte: u8) {
    match byte {
        b'"' => out.extend_from_slice(br#"\""#),
        b'\\' => out.extend_from_slice(br#"\\"#),
        0x08 => out.extend_from_slice(br#"\b"#),
        b'\t' => out.extend_from_slice(br#"\t"#),
        b'\n' => out.extend_from_slice(br#"\n"#),
        0x0B => out.extend_from_slice(br#"\v"#),
        0x0C => out.extend_from_slice(br#"\f"#),
        b'\r' => out.extend_from_slice(br#"\r"#),
        0x07 => out.extend_from_slice(br#"\a"#),
        0x1B => out.extend_from_slice(br#"\e"#),
        b if b < 0x20 || b == 0x7F => push_octal_escape(out, b),
        b => out.push(b),
    }
}

/// Format a Rust string as an Emacs Lisp string literal, preserving byte-level
/// sentinels via lossy UTF-8 conversion when invalid byte sequences occur.
pub(crate) fn format_lisp_string(s: &str) -> String {
    String::from_utf8_lossy(&format_lisp_string_bytes(s)).into_owned()
}

/// Format a Rust string as an Emacs Lisp string literal byte sequence.
pub(crate) fn format_lisp_string_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(b'"');

    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        let code = ch as u32;
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&code) {
            let byte = (code - UNIBYTE_BYTE_SENTINEL_BASE) as u8;
            push_unibyte_literal_byte(&mut out, byte);
            continue;
        }

        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&code) {
            let byte = (code - RAW_BYTE_SENTINEL_BASE) as u8;
            push_octal_escape(&mut out, byte);
            continue;
        }

        if code == EXT_SEQ_PREFIX {
            if let Some(bytes) = decode_extended_sequence(&mut chars) {
                for b in bytes {
                    push_escaped_literal_byte(&mut out, b);
                }
                continue;
            }
        }

        match ch {
            // GNU Emacs: only `"` and `\` are always escaped in prin1.
            // `\n` and `\f` are only escaped when print-escape-newlines is t
            // (default nil). Control chars only escaped when
            // print-escape-control-characters is t (default nil in batch mode).
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br#"\\"#),
            _ => {
                let mut tmp = [0u8; 4];
                let bytes = ch.encode_utf8(&mut tmp).as_bytes();
                out.extend_from_slice(bytes);
            }
        }
    }

    out.push(b'"');
    out
}
#[cfg(test)]
#[path = "string_escape_test.rs"]
mod tests;
