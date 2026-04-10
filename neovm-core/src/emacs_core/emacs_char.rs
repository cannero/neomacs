//! Emacs internal character encoding.
//!
//! Implements the encoding described in GNU Emacs `character.h` / `character.c`.
//! This is a UTF-8 superset: standard Unicode code points use normal UTF-8,
//! while raw bytes 0x80..0xFF are represented as "eight-bit" characters in the
//! range 0x3FFF00..0x3FFFFF, encoded with overlong 2-byte sequences (C0/C1
//! lead bytes) that are illegal in standard UTF-8.
//!
//! Encoding table:
//!
//! | Character Range         | Bytes | Encoding                                        |
//! |-------------------------|-------|-------------------------------------------------|
//! | U+0000 .. U+007F        | 1     | 0xxxxxxx  (ASCII)                               |
//! | U+0080 .. U+07FF        | 2     | 110xxxxx 10xxxxxx  (standard UTF-8)              |
//! | U+0800 .. U+FFFF        | 3     | 1110xxxx 10xxxxxx 10xxxxxx                       |
//! | U+10000 .. U+1FFFFF     | 4     | 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx              |
//! | U+200000 .. U+3FFF7F    | 5     | 11111000 10xxxxxx 10xxxxxx 10xxxxxx 10xxxxxx     |
//! | Raw byte 0x80..0xFF     | 2     | 1100000x 10xxxxxx  (overlong, NOT valid UTF-8)   |

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum valid Emacs character code (0x3FFFFF).
pub const MAX_CHAR: u32 = 0x3F_FFFF;

/// Maximum Unicode code point (0x10FFFF).
pub const MAX_UNICODE_CHAR: u32 = 0x10_FFFF;

/// Maximum 5-byte encoded character (0x3FFF7F).
/// Characters above this (0x3FFF80..0x3FFFFF) are raw-byte ("eight-bit") characters.
pub const MAX_5_BYTE_CHAR: u32 = 0x3F_FF7F;

/// Maximum bytes needed to encode any Emacs character in multibyte form.
pub const MAX_MULTIBYTE_LENGTH: usize = 5;

/// Start of the raw-byte character range.
const BYTE8_OFFSET: u32 = 0x3F_FF00;

// ---------------------------------------------------------------------------
// Raw-byte predicates and converters
// ---------------------------------------------------------------------------

/// Return `true` if `c` is a raw-byte ("eight-bit") character,
/// i.e. in the range 0x3FFF00..0x3FFFFF.
#[inline]
pub fn char_byte8_p(c: u32) -> bool {
    c > MAX_5_BYTE_CHAR
}

/// Convert a raw byte (0x00..0xFF) to its Emacs character code.
///
/// For bytes 0x00..0x7F the result is the byte itself (ASCII).
/// For bytes 0x80..0xFF the result is `byte + 0x3FFF00`.
#[inline]
pub fn byte8_to_char(byte: u8) -> u32 {
    if byte >= 0x80 {
        byte as u32 + BYTE8_OFFSET
    } else {
        byte as u32
    }
}

/// Convert a raw-byte character code back to its byte value.
///
/// The caller must ensure `c` is a raw-byte character (i.e. `char_byte8_p(c)`
/// is true). For ASCII-range characters this also works (returns low byte).
#[inline]
pub fn char_to_byte8(c: u32) -> u8 {
    if char_byte8_p(c) {
        (c - BYTE8_OFFSET) as u8
    } else {
        // ASCII or shouldn't be called, but be safe.
        c as u8
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

/// Return the number of bytes needed to encode character `c` in Emacs
/// multibyte form.
#[inline]
pub fn char_bytes(c: u32) -> usize {
    if c < 0x80 {
        1
    } else if c < 0x800 {
        2
    } else if c < 0x1_0000 {
        3
    } else if c < 0x20_0000 {
        4
    } else if c <= MAX_5_BYTE_CHAR {
        5
    } else {
        // Raw byte: encoded as overlong 2-byte sequence.
        2
    }
}

/// Encode character `c` into `buf` and return the number of bytes written.
///
/// `buf` must be at least [`MAX_MULTIBYTE_LENGTH`] bytes long.
/// This mirrors GNU `CHAR_STRING`.
pub fn char_string(c: u32, buf: &mut [u8]) -> usize {
    if c < 0x80 {
        // ASCII
        buf[0] = c as u8;
        1
    } else if c < 0x800 {
        // 2-byte standard UTF-8
        buf[0] = 0xC0 | (c >> 6) as u8;
        buf[1] = 0x80 | (c & 0x3F) as u8;
        2
    } else if c < 0x1_0000 {
        // 3-byte
        buf[0] = 0xE0 | (c >> 12) as u8;
        buf[1] = 0x80 | ((c >> 6) & 0x3F) as u8;
        buf[2] = 0x80 | (c & 0x3F) as u8;
        3
    } else if c < 0x20_0000 {
        // 4-byte
        buf[0] = 0xF0 | (c >> 18) as u8;
        buf[1] = 0x80 | ((c >> 12) & 0x3F) as u8;
        buf[2] = 0x80 | ((c >> 6) & 0x3F) as u8;
        buf[3] = 0x80 | (c & 0x3F) as u8;
        4
    } else if c <= MAX_5_BYTE_CHAR {
        // 5-byte (extended Emacs range, not raw byte)
        buf[0] = 0xF8;
        buf[1] = 0x80 | ((c >> 18) & 0x3F) as u8;
        buf[2] = 0x80 | ((c >> 12) & 0x3F) as u8;
        buf[3] = 0x80 | ((c >> 6) & 0x3F) as u8;
        buf[4] = 0x80 | (c & 0x3F) as u8;
        5
    } else {
        // Raw byte (0x3FFF80..0x3FFFFF) → overlong 2-byte.
        // Raw bytes 0x80..0xFF are encoded as if they were 0x00..0x7F in
        // overlong form:  byte 0x80 → [C0,80], byte 0xBF → [C0,BF],
        //                 byte 0xC0 → [C1,80], byte 0xFF → [C1,BF].
        let val = char_to_byte8(c).wrapping_sub(0x80);
        buf[0] = 0xC0 | (val >> 6);
        buf[1] = 0x80 | (val & 0x3F);
        2
    }
}

// ---------------------------------------------------------------------------
// Decoding helpers
// ---------------------------------------------------------------------------

/// Return `true` if `lead` is the start of a raw-byte overlong sequence.
///
/// Raw bytes are encoded as `[0xC0, 0x80..0xBF]` or `[0xC1, 0x80..0xBF]`,
/// which are the overlong representations of U+0000..U+007F and U+0040..U+007F
/// respectively. In Emacs encoding these represent raw bytes 0x80..0xFF.
#[inline]
fn is_raw_byte_lead(lead: u8) -> bool {
    lead == 0xC0 || lead == 0xC1
}

/// Decode one character from `bytes`, returning `(char_code, bytes_consumed)`.
///
/// This mirrors GNU `STRING_CHAR_AND_LENGTH`. The input must be a valid Emacs
/// multibyte sequence (no bounds checking on continuation bytes beyond what the
/// lead byte promises).
pub fn string_char(bytes: &[u8]) -> (u32, usize) {
    debug_assert!(!bytes.is_empty());
    let b0 = bytes[0];

    if b0 < 0x80 {
        // ASCII
        (b0 as u32, 1)
    } else if is_raw_byte_lead(b0) && bytes.len() >= 2 && (bytes[1] & 0xC0) == 0x80 {
        // Raw-byte overlong: 2 bytes → eight-bit character.
        let raw = ((b0 & 0x01) << 6) | (bytes[1] & 0x3F);
        (byte8_to_char(raw | 0x80), 2)
    } else if b0 < 0xE0 && bytes.len() >= 2 && (bytes[1] & 0xC0) == 0x80 {
        // 2-byte standard UTF-8
        let c = ((b0 as u32 & 0x1F) << 6) | (bytes[1] as u32 & 0x3F);
        (c, 2)
    } else if b0 < 0xF0 && bytes.len() >= 3
        && (bytes[1] & 0xC0) == 0x80 && (bytes[2] & 0xC0) == 0x80
    {
        // 3-byte
        let c = ((b0 as u32 & 0x0F) << 12)
            | ((bytes[1] as u32 & 0x3F) << 6)
            | (bytes[2] as u32 & 0x3F);
        (c, 3)
    } else if b0 < 0xF8 && bytes.len() >= 4
        && (bytes[1] & 0xC0) == 0x80 && (bytes[2] & 0xC0) == 0x80
        && (bytes[3] & 0xC0) == 0x80
    {
        // 4-byte
        let c = ((b0 as u32 & 0x07) << 18)
            | ((bytes[1] as u32 & 0x3F) << 12)
            | ((bytes[2] as u32 & 0x3F) << 6)
            | (bytes[3] as u32 & 0x3F);
        (c, 4)
    } else if b0 == 0xF8 && bytes.len() >= 5
        && (bytes[1] & 0xC0) == 0x80 && (bytes[2] & 0xC0) == 0x80
        && (bytes[3] & 0xC0) == 0x80 && (bytes[4] & 0xC0) == 0x80
    {
        // 5-byte (F8 lead, Emacs extension)
        let c = ((bytes[1] as u32 & 0x3F) << 18)
            | ((bytes[2] as u32 & 0x3F) << 12)
            | ((bytes[3] as u32 & 0x3F) << 6)
            | (bytes[4] as u32 & 0x3F);
        (c, 5)
    } else {
        // Invalid or truncated sequence — treat lead byte as raw byte
        (byte8_to_char(b0), 1)
    }
}

// ---------------------------------------------------------------------------
// Higher-level utilities
// ---------------------------------------------------------------------------

/// Count the number of characters in a multibyte byte sequence.
pub fn chars_in_multibyte(bytes: &[u8]) -> usize {
    let mut count = 0usize;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let (_, len) = string_char(&bytes[pos..]);
        pos += len;
        count += 1;
    }
    count
}

/// Convert a character index to a byte offset.
///
/// Returns the byte position of the `char_idx`-th character (0-based).
/// If `char_idx` is beyond the end of the string, returns `bytes.len()`.
pub fn char_to_byte_pos(bytes: &[u8], char_idx: usize) -> usize {
    let mut pos = 0usize;
    let mut ci = 0usize;
    while ci < char_idx && pos < bytes.len() {
        let (_, len) = string_char(&bytes[pos..]);
        pos += len;
        ci += 1;
    }
    pos
}

/// Convert a byte offset to a character index.
///
/// `byte_pos` should fall on a character boundary. Returns the number of
/// characters before that byte position.
pub fn byte_to_char_pos(bytes: &[u8], byte_pos: usize) -> usize {
    let mut pos = 0usize;
    let mut ci = 0usize;
    while pos < byte_pos && pos < bytes.len() {
        let (_, len) = string_char(&bytes[pos..]);
        pos += len;
        ci += 1;
    }
    ci
}

/// If the byte sequence is valid UTF-8 (i.e. contains no raw-byte overlong
/// sequences), return it as a `&str`. Otherwise return `None`.
pub fn try_as_utf8(bytes: &[u8]) -> Option<&str> {
    // A quick check: if std UTF-8 validation passes AND there are no C0/C1
    // lead bytes, it is plain UTF-8. However, C0/C1 leads are always invalid
    // UTF-8 anyway, so `std::str::from_utf8` already rejects them.
    std::str::from_utf8(bytes).ok()
}

/// Convert an Emacs-encoded byte sequence to a UTF-8 `String`, replacing any
/// raw-byte characters with U+FFFD (REPLACEMENT CHARACTER).
pub fn to_utf8_lossy(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let (c, len) = string_char(&bytes[pos..]);
        if char_byte8_p(c) {
            out.push('\u{FFFD}');
        } else if let Some(ch) = char::from_u32(c) {
            out.push(ch);
        } else {
            out.push('\u{FFFD}');
        }
        pos += len;
    }
    out
}

/// Convert a UTF-8 string to Emacs internal encoding.
///
/// For standard Unicode this is a no-op (UTF-8 is a subset of Emacs encoding).
/// The returned `Vec<u8>` is byte-for-byte identical to the input's UTF-8
/// representation.
pub fn utf8_to_emacs(s: &str) -> Vec<u8> {
    // Standard UTF-8 is already valid Emacs encoding. No transformation needed.
    s.as_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Tests (in separate file per project convention)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "emacs_char_test.rs"]
mod tests;
