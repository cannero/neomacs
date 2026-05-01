//! Character encoding, multibyte support, and character utilities.
//!
//! Neomacs uses UTF-8 internally.  This module provides Emacs-compatible
//! character classification, width calculation, and encoding conversion
//! APIs.

use crate::emacs_core::intern::resolve_sym;
// encoding.rs: sentinel imports removed; using emacs_char + LispString directly
use crate::emacs_core::value::{StringTextPropertyRun, Value, ValueKind};
use encoding_rs::{BIG5, GBK};

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

// Zero-width characters — non-spacing marks, enclosing combining
// marks, formatting controls, Hangul Jamo medial/final, and ZWJ/VS
// sequences. Transcribed from `lisp/international/characters.el`
// (the `;; 0: non-spacing, enclosing combining, …` block). This is
// the authoritative default char-width-table entry=0 set used by
// GNU Emacs. Must stay sorted ascending by `start` for the binary
// search in `codepoint_in_sorted_ranges` to work.
const ZERO_WIDTH_RANGES: &[(u32, u32)] = &[
    (0x0300, 0x036F),
    (0x0483, 0x0489),
    (0x0591, 0x05BD),
    (0x05BF, 0x05BF),
    (0x05C1, 0x05C2),
    (0x05C4, 0x05C5),
    (0x05C7, 0x05C7),
    (0x0600, 0x0605),
    (0x0610, 0x061C),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06E4),
    (0x06E7, 0x06E8),
    (0x06EA, 0x06ED),
    (0x070F, 0x070F),
    (0x0711, 0x0711),
    (0x0730, 0x074A),
    (0x07A6, 0x07B0),
    (0x07EB, 0x07F3),
    (0x0816, 0x0823),
    (0x0825, 0x082D),
    (0x0859, 0x085B),
    (0x08D4, 0x0902),
    (0x093A, 0x093A),
    (0x093C, 0x093C),
    (0x0941, 0x0948),
    (0x094D, 0x094D),
    (0x0951, 0x0957),
    (0x0962, 0x0963),
    (0x0981, 0x0981),
    (0x09BC, 0x09BC),
    (0x09C1, 0x09C4),
    (0x09CD, 0x09CD),
    (0x09E2, 0x09E3),
    (0x0A01, 0x0A02),
    (0x0A3C, 0x0A3C),
    (0x0A41, 0x0A4D),
    (0x0A51, 0x0A51),
    (0x0A70, 0x0A71),
    (0x0A75, 0x0A75),
    (0x0A81, 0x0A82),
    (0x0ABC, 0x0ABC),
    (0x0AC1, 0x0AC8),
    (0x0ACD, 0x0ACD),
    (0x0AE2, 0x0AE3),
    (0x0B01, 0x0B01),
    (0x0B3C, 0x0B3C),
    (0x0B3F, 0x0B3F),
    (0x0B41, 0x0B44),
    (0x0B4D, 0x0B56),
    (0x0B62, 0x0B63),
    (0x0B82, 0x0B82),
    (0x0BC0, 0x0BC0),
    (0x0BCD, 0x0BCD),
    (0x0C00, 0x0C00),
    (0x0C3E, 0x0C40),
    (0x0C46, 0x0C56),
    (0x0C62, 0x0C63),
    (0x0C81, 0x0C81),
    (0x0CBC, 0x0CBC),
    (0x0CCC, 0x0CCD),
    (0x0CE2, 0x0CE3),
    (0x0D01, 0x0D01),
    (0x0D41, 0x0D44),
    (0x0D4D, 0x0D4D),
    (0x0D62, 0x0D63),
    (0x0D81, 0x0D81),
    (0x0DCA, 0x0DCA),
    (0x0DD2, 0x0DD6),
    (0x0E31, 0x0E31),
    (0x0E34, 0x0E3A),
    (0x0E47, 0x0E4E),
    (0x0EB1, 0x0EB1),
    (0x0EB4, 0x0EBC),
    (0x0EC8, 0x0ECD),
    (0x0F18, 0x0F19),
    (0x0F35, 0x0F35),
    (0x0F37, 0x0F37),
    (0x0F39, 0x0F39),
    (0x0F71, 0x0F7E),
    (0x0F80, 0x0F84),
    (0x0F86, 0x0F87),
    (0x0F8D, 0x0FBC),
    (0x0FC6, 0x0FC6),
    (0x102D, 0x1030),
    (0x1032, 0x1037),
    (0x1039, 0x103A),
    (0x103D, 0x103E),
    (0x1058, 0x1059),
    (0x105E, 0x1060),
    (0x1071, 0x1074),
    (0x1082, 0x1082),
    (0x1085, 0x1086),
    (0x108D, 0x108D),
    (0x109D, 0x109D),
    (0x1160, 0x11FF),
    (0x135D, 0x135F),
    (0x1712, 0x1714),
    (0x1732, 0x1734),
    (0x1752, 0x1753),
    (0x1772, 0x1773),
    (0x17B4, 0x17B5),
    (0x17B7, 0x17BD),
    (0x17C6, 0x17C6),
    (0x17C9, 0x17D3),
    (0x17DD, 0x17DD),
    (0x180B, 0x180E),
    (0x1885, 0x1886),
    (0x18A9, 0x18A9),
    (0x1920, 0x1922),
    (0x1927, 0x1928),
    (0x1932, 0x1932),
    (0x1939, 0x193B),
    (0x1A17, 0x1A18),
    (0x1A1B, 0x1A1B),
    (0x1A56, 0x1A56),
    (0x1A58, 0x1A5E),
    (0x1A60, 0x1A60),
    (0x1A62, 0x1A62),
    (0x1A65, 0x1A6C),
    (0x1A73, 0x1A7C),
    (0x1A7F, 0x1A7F),
    (0x1AB0, 0x1AC0),
    (0x1B00, 0x1B03),
    (0x1B34, 0x1B34),
    (0x1B36, 0x1B3A),
    (0x1B3C, 0x1B3C),
    (0x1B42, 0x1B42),
    (0x1B6B, 0x1B73),
    (0x1B80, 0x1B81),
    (0x1BA2, 0x1BA5),
    (0x1BA8, 0x1BA9),
    (0x1BAB, 0x1BAD),
    (0x1BE6, 0x1BE6),
    (0x1BE8, 0x1BE9),
    (0x1BED, 0x1BED),
    (0x1BEF, 0x1BF1),
    (0x1C2C, 0x1C33),
    (0x1C36, 0x1C37),
    (0x1CD0, 0x1CD2),
    (0x1CD4, 0x1CE0),
    (0x1CE2, 0x1CE8),
    (0x1CED, 0x1CED),
    (0x1CF4, 0x1CF4),
    (0x1CF8, 0x1CF9),
    (0x1DC0, 0x1DFF),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x206F),
    (0x20D0, 0x20F0),
    (0x2CEF, 0x2CF1),
    (0x2D7F, 0x2D7F),
    (0x2DE0, 0x2DFF),
    (0xA66F, 0xA672),
    (0xA674, 0xA69F),
    (0xA6F0, 0xA6F1),
    (0xA802, 0xA802),
    (0xA806, 0xA806),
    (0xA80B, 0xA80B),
    (0xA825, 0xA826),
    (0xA82C, 0xA82C),
    (0xA8C4, 0xA8C5),
    (0xA8E0, 0xA8F1),
    (0xA926, 0xA92D),
    (0xA947, 0xA951),
    (0xA980, 0xA9B3),
    (0xA9B6, 0xA9B9),
    (0xA9BC, 0xA9BC),
    (0xA9E5, 0xA9E5),
    (0xAA29, 0xAA2E),
    (0xAA31, 0xAA32),
    (0xAA35, 0xAA36),
    (0xAA43, 0xAA43),
    (0xAA4C, 0xAA4C),
    (0xAA7C, 0xAA7C),
    (0xAAB0, 0xAAB0),
    (0xAAB2, 0xAAB4),
    (0xAAB7, 0xAAB8),
    (0xAABE, 0xAABF),
    (0xAAC1, 0xAAC1),
    (0xAAEC, 0xAAED),
    (0xAAF6, 0xAAF6),
    (0xABE5, 0xABE5),
    (0xABE8, 0xABE8),
    (0xABED, 0xABED),
    (0xD7B0, 0xD7FB),
    (0xFB1E, 0xFB1E),
    (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x101FD, 0x101FD),
    (0x102E0, 0x102E0),
    (0x10376, 0x1037A),
    (0x10A01, 0x10A0F),
    (0x10A38, 0x10A3F),
    (0x10AE5, 0x10AE6),
    (0x10D69, 0x10D6D),
    (0x10EAB, 0x10EAC),
    (0x10EFC, 0x10EFF),
    (0x11001, 0x11001),
    (0x11038, 0x11046),
    (0x1107F, 0x11081),
    (0x110B3, 0x110B6),
    (0x110B9, 0x110BA),
    (0x110BD, 0x110BD),
    (0x11100, 0x11102),
    (0x11127, 0x1112B),
    (0x1112D, 0x11134),
    (0x11173, 0x11173),
    (0x11180, 0x11181),
    (0x111B6, 0x111BE),
    (0x111CA, 0x111CC),
    (0x111CF, 0x111CF),
    (0x1122F, 0x11231),
    (0x11234, 0x11234),
    (0x11236, 0x11237),
    (0x1123E, 0x1123E),
    (0x112DF, 0x112DF),
    (0x112E3, 0x112EA),
    (0x11300, 0x11301),
    (0x1133C, 0x1133C),
    (0x11340, 0x11340),
    (0x11366, 0x1136C),
    (0x11370, 0x11374),
    (0x113BB, 0x113C0),
    (0x113CE, 0x113CE),
    (0x113D0, 0x113D0),
    (0x113D2, 0x113D2),
    (0x113E1, 0x113E2),
    (0x11438, 0x1143F),
    (0x11442, 0x11444),
    (0x11446, 0x11446),
    (0x114B3, 0x114B8),
    (0x114BA, 0x114C0),
    (0x114C2, 0x114C3),
    (0x115B2, 0x115B5),
    (0x115BC, 0x115BD),
    (0x115BF, 0x115C0),
    (0x115DC, 0x115DD),
    (0x11633, 0x1163A),
    (0x1163D, 0x1163D),
    (0x1163F, 0x11640),
    (0x116AB, 0x116AB),
    (0x116AD, 0x116AD),
    (0x116B0, 0x116B5),
    (0x116B7, 0x116B7),
    (0x1171D, 0x1171F),
    (0x11722, 0x11725),
    (0x11727, 0x1172B),
    (0x1193B, 0x1193C),
    (0x1193E, 0x1193E),
    (0x11943, 0x11943),
    (0x11C30, 0x11C36),
    (0x11C38, 0x11C3D),
    (0x11C92, 0x11CA7),
    (0x11CAA, 0x11CB0),
    (0x11CB2, 0x11CB3),
    (0x11CB5, 0x11CB6),
    (0x11F5A, 0x11F5A),
    (0x13430, 0x13440),
    (0x13447, 0x13455),
    (0x1611E, 0x16129),
    (0x1612D, 0x1612F),
    (0x16AF0, 0x16AF4),
    (0x16B30, 0x16B36),
    (0x16F8F, 0x16F92),
    (0x16FE4, 0x16FE4),
    (0x1BC9D, 0x1BC9E),
    (0x1BCA0, 0x1BCA3),
    (0x1CF00, 0x1CF02),
    (0x1D167, 0x1D169),
    (0x1D173, 0x1D182),
    (0x1D185, 0x1D18B),
    (0x1D1AA, 0x1D1AD),
    (0x1D242, 0x1D244),
    (0x1DA00, 0x1DA36),
    (0x1DA3B, 0x1DA6C),
    (0x1DA75, 0x1DA75),
    (0x1DA84, 0x1DA84),
    (0x1DA9B, 0x1DA9F),
    (0x1DAA1, 0x1DAAF),
    (0x1E000, 0x1E006),
    (0x1E008, 0x1E018),
    (0x1E01B, 0x1E021),
    (0x1E023, 0x1E024),
    (0x1E026, 0x1E02A),
    (0x1E5EE, 0x1E5EF),
    (0x1E8D0, 0x1E8D6),
    (0x1E944, 0x1E94A),
    (0xE0001, 0xE01EF),
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
    // GNU `CHARACTER_WIDTH` returns 1 for printable ASCII before the
    // char-width-table lookup.
    if cp < 0x80 {
        return 1;
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
    match coding_system
        .strip_suffix("-unix")
        .or_else(|| coding_system.strip_suffix("-dos"))
        .or_else(|| coding_system.strip_suffix("-mac"))
        .unwrap_or(coding_system)
    {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
        "latin-5" | "iso-8859-9" | "iso-latin-5" => "iso-latin-5",
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => "iso-latin-9",
        "cn-gb-2312" | "euc-china" | "euc-cn" | "cn-gb" | "gb2312" | "chinese-iso-8bit" => {
            "chinese-iso-8bit"
        }
        "big5" | "cn-big5" | "cp950" | "chinese-big5" => "chinese-big5",
        "big5-hkscs" | "cn-big5-hkscs" | "chinese-big5-hkscs" => "chinese-big5-hkscs",
        "emacs-internal" => "utf-8-emacs",
        family => family,
    }
}

#[derive(Clone, Copy)]
enum Utf16Endian {
    Big,
    Little,
}

fn utf16_coding_variant(coding_system: &str) -> Option<(Utf16Endian, bool)> {
    let base = coding_system
        .strip_suffix("-unix")
        .or_else(|| coding_system.strip_suffix("-dos"))
        .or_else(|| coding_system.strip_suffix("-mac"))
        .unwrap_or(coding_system);
    match base {
        "utf-16" | "utf-16-be" | "utf-16be-with-signature" => Some((Utf16Endian::Big, true)),
        "utf-16-le" | "utf-16le-with-signature" => Some((Utf16Endian::Little, true)),
        "utf-16be" => Some((Utf16Endian::Big, false)),
        "utf-16le" => Some((Utf16Endian::Little, false)),
        _ => None,
    }
}

fn push_utf16_unit(out: &mut Vec<u8>, endian: Utf16Endian, unit: u16) {
    match endian {
        Utf16Endian::Big => out.extend_from_slice(&unit.to_be_bytes()),
        Utf16Endian::Little => out.extend_from_slice(&unit.to_le_bytes()),
    }
}

fn push_utf16_codepoint(out: &mut Vec<u8>, endian: Utf16Endian, code: u32) {
    let code = if crate::emacs_core::emacs_char::char_byte8_p(code) {
        crate::emacs_core::emacs_char::char_to_byte8(code) as u32
    } else if (0xD800..=0xDFFF).contains(&code) || code > 0x10FFFF {
        0xFFFD
    } else {
        code
    };

    if code <= 0xFFFF {
        push_utf16_unit(out, endian, code as u16);
    } else {
        let scalar = code - 0x1_0000;
        let high = 0xD800 | ((scalar >> 10) as u16);
        let low = 0xDC00 | ((scalar & 0x3FF) as u16);
        push_utf16_unit(out, endian, high);
        push_utf16_unit(out, endian, low);
    }
}

fn encode_utf16_lisp_string(
    s: &crate::heap_types::LispString,
    endian: Utf16Endian,
    bom: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.sbytes() * 2 + 2);
    if bom {
        match endian {
            Utf16Endian::Big => out.extend_from_slice(&[0xFE, 0xFF]),
            Utf16Endian::Little => out.extend_from_slice(&[0xFF, 0xFE]),
        }
    }

    if s.is_multibyte() {
        let bytes = s.as_bytes();
        let mut pos = 0usize;
        while pos < bytes.len() {
            let (code, len) = crate::emacs_core::emacs_char::string_char(&bytes[pos..]);
            push_utf16_codepoint(&mut out, endian, code);
            pos += len;
        }
    } else {
        for &byte in s.as_bytes() {
            push_utf16_codepoint(&mut out, endian, byte as u32);
        }
    }

    out
}

fn encode_utf16_text(s: &str, endian: Utf16Endian, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    if bom {
        match endian {
            Utf16Endian::Big => out.extend_from_slice(&[0xFE, 0xFF]),
            Utf16Endian::Little => out.extend_from_slice(&[0xFF, 0xFE]),
        }
    }
    for code in s.chars().map(|ch| ch as u32) {
        push_utf16_codepoint(&mut out, endian, code);
    }
    out
}

fn decode_utf16_bytes(bytes: &[u8], default_endian: Utf16Endian) -> String {
    let (endian, body) = match bytes {
        [0xFE, 0xFF, rest @ ..] => (Utf16Endian::Big, rest),
        [0xFF, 0xFE, rest @ ..] => (Utf16Endian::Little, rest),
        _ => (default_endian, bytes),
    };

    let mut units = Vec::with_capacity(body.len() / 2);
    let mut chunks = body.chunks_exact(2);
    for chunk in &mut chunks {
        let pair = [chunk[0], chunk[1]];
        units.push(match endian {
            Utf16Endian::Big => u16::from_be_bytes(pair),
            Utf16Endian::Little => u16::from_le_bytes(pair),
        });
    }
    if !chunks.remainder().is_empty() {
        units.push(0xFFFD);
    }

    std::char::decode_utf16(units)
        .map(|item| item.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn decode_single_byte_family_char(family: &str, byte: u8) -> Option<char> {
    match family {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => Some(byte as char),
        "iso-latin-5" => Some(match byte {
            0xD0 => '\u{011E}',
            0xDD => '\u{0130}',
            0xDE => '\u{015E}',
            0xF0 => '\u{011F}',
            0xFD => '\u{0131}',
            0xFE => '\u{015F}',
            _ => byte as char,
        }),
        "iso-latin-9" => Some(match byte {
            0xA4 => '\u{20AC}',
            0xA6 => '\u{0160}',
            0xA8 => '\u{0161}',
            0xB4 => '\u{017D}',
            0xB8 => '\u{017E}',
            0xBC => '\u{0152}',
            0xBD => '\u{0153}',
            0xBE => '\u{0178}',
            _ => byte as char,
        }),
        _ => None,
    }
}

fn encode_single_byte_family_char(family: &str, code: u32) -> Option<u8> {
    match family {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => (code <= 0xFF).then_some(code as u8),
        "iso-latin-5" => match code {
            0x011E => Some(0xD0),
            0x0130 => Some(0xDD),
            0x015E => Some(0xDE),
            0x011F => Some(0xF0),
            0x0131 => Some(0xFD),
            0x015F => Some(0xFE),
            _ if code <= 0xFF => Some(code as u8),
            _ => None,
        },
        "iso-latin-9" => match code {
            0x20AC => Some(0xA4),
            0x0160 => Some(0xA6),
            0x0161 => Some(0xA8),
            0x017D => Some(0xB4),
            0x017E => Some(0xB8),
            0x0152 => Some(0xBC),
            0x0153 => Some(0xBD),
            0x0178 => Some(0xBE),
            _ if code <= 0xFF => Some(code as u8),
            _ => None,
        },
        _ => None,
    }
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
        let code = crate::emacs_core::emacs_char::unibyte_to_char(byte);
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
    if let Some((endian, bom)) = utf16_coding_variant(coding_system) {
        return encode_utf16_lisp_string(s, endian, bom);
    }

    let family = coding_system_family(coding_system);
    if matches!(
        family,
        "utf-8" | "utf-8-emacs" | "undecided" | "prefer-utf-8"
    ) || is_byte_preserving_coding_system(coding_system)
    {
        return encode_eol_bytes(s.as_bytes(), coding_system);
    }

    if matches!(
        family,
        "chinese-iso-8bit" | "chinese-big5" | "chinese-big5-hkscs"
    ) {
        let text = decode_utf8_emacs_bytes(s.as_bytes());
        return encode_string(&text, coding_system);
    }

    let mut out = Vec::with_capacity(s.sbytes());
    let mut push_encoded = |code: u32| match family {
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "iso-latin-5" | "iso-latin-9" => {
            if let Some(byte) = encode_single_byte_family_char(family, code) {
                out.push(byte);
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
    if let Some((endian, bom)) = utf16_coding_variant(coding_system) {
        return encode_utf16_text(&eol_text, endian, bom);
    }

    match coding_system_family(coding_system) {
        "utf-8" | "utf-8-unix" | "utf-8-dos" | "utf-8-mac" | "utf-8-emacs" => {
            encode_utf8_emacs_text(&eol_text)
        }
        "chinese-big5" | "chinese-big5-hkscs" => {
            let (encoded, _, _) = BIG5.encode(&eol_text);
            encoded.into_owned()
        }
        "chinese-iso-8bit" => {
            let (encoded, _, _) = GBK.encode(&eol_text);
            encoded.into_owned()
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "iso-latin-5" | "iso-latin-9" => eol_text
            .chars()
            .map(|c| {
                encode_single_byte_family_char(coding_system_family(coding_system), c as u32)
                    .unwrap_or(b'?')
            })
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
    if let Some((endian, _bom)) = utf16_coding_variant(coding_system) {
        return decode_utf16_bytes(bytes, endian);
    }

    let bytes = decode_eol_text(bytes, coding_system);
    match coding_system_family(coding_system) {
        "utf-8" | "utf-8-emacs" => decode_utf8_emacs_bytes(&bytes),
        "chinese-big5" | "chinese-big5-hkscs" => {
            let (decoded, _, _) = BIG5.decode(&bytes);
            decoded.into_owned()
        }
        "chinese-iso-8bit" => {
            let (decoded, _, _) = GBK.decode(&bytes);
            decoded.into_owned()
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "iso-latin-5" | "iso-latin-9" => bytes
            .iter()
            .map(|&b| {
                decode_single_byte_family_char(coding_system_family(coding_system), b)
                    .unwrap_or('\u{FFFD}')
            })
            .collect(),
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
        let code = crate::emacs_core::emacs_char::unibyte_to_char(b);
        push_emacs_utf8_decoded_char(&mut out, code);
    }
    out
}

fn charset_property_runs(text: &str, charset: &str) -> Vec<StringTextPropertyRun> {
    let mut char_count = 0usize;
    let mut first_non_ascii = None;
    for (idx, ch) in text.chars().enumerate() {
        if first_non_ascii.is_none() && (ch as u32) > 0x7f {
            first_non_ascii = Some(idx);
        }
        char_count = idx + 1;
    }

    let Some(first_non_ascii) = first_non_ascii else {
        return Vec::new();
    };
    let start = if charset == "iso-8859-1" {
        0
    } else {
        first_non_ascii
    };

    vec![StringTextPropertyRun {
        start,
        end: char_count,
        plist: Value::list(vec![Value::symbol("charset"), Value::symbol(charset)]),
    }]
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

fn expect_range_args(
    name: &str,
    args: &[Value],
    min: usize,
    max: usize,
) -> Result<(), crate::emacs_core::error::Flow> {
    if args.len() < min || args.len() > max {
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
    crate::emacs_core::coding::CodingSystemManager::new().is_known_or_derived(name)
}

fn validate_coding_system(
    name: &str,
    arg: Value,
    known: impl FnOnce(&str) -> bool,
) -> Result<(), crate::emacs_core::error::Flow> {
    if known(name) {
        Ok(())
    } else {
        Err(signal("coding-system-error", vec![arg]))
    }
}

fn coding_string_nocopy(args: &[Value]) -> bool {
    args.get(2).is_some_and(|value| value.is_truthy())
}

fn copy_lisp_string_value(value: Value) -> Result<Value, crate::emacs_core::error::Flow> {
    let string = value
        .as_lisp_string()
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), value]))?;
    Ok(Value::heap_string(string.clone()))
}

fn context_coding_name(
    ctx: &crate::emacs_core::eval::Context,
    coding_arg: Value,
) -> Result<String, crate::emacs_core::error::Flow> {
    let name = match coding_arg.kind() {
        ValueKind::Nil => "no-conversion".to_owned(),
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), coding_arg],
            ));
        }
    };
    validate_coding_system(&name, coding_arg, |candidate| {
        ctx.coding_systems.is_known_or_derived(candidate)
    })?;
    Ok(name)
}

fn canonical_context_coding_name(ctx: &crate::emacs_core::eval::Context, name: &str) -> String {
    ctx.coding_systems
        .canonical_runtime_name(name)
        .unwrap_or_else(|| name.to_owned())
}

fn coding_region_destination(
    arg: Option<Value>,
) -> Result<Option<Option<crate::buffer::BufferId>>, crate::emacs_core::error::Flow> {
    let Some(value) = arg else {
        return Ok(Some(None));
    };
    if value.is_nil() {
        return Ok(Some(None));
    }
    if value.is_t() {
        return Ok(None);
    }
    if let Some(buffer_id) = value.as_buffer_id() {
        Ok(Some(Some(buffer_id)))
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), value],
        ))
    }
}

fn transformed_region_string(
    source: crate::heap_types::LispString,
    coding: &str,
    encode: bool,
) -> Result<Value, crate::emacs_core::error::Flow> {
    if encode {
        let bytes = encode_lisp_string(&source, coding);
        Ok(Value::heap_string(
            crate::heap_types::LispString::from_unibyte(bytes),
        ))
    } else {
        builtin_decode_coding_string_with_known(
            vec![
                Value::heap_string(source),
                Value::symbol(coding),
                Value::NIL,
                Value::NIL,
            ],
            |_| true,
        )
    }
}

fn insert_coding_result(
    ctx: &mut crate::emacs_core::eval::Context,
    buffer_id: crate::buffer::BufferId,
    text: &crate::heap_types::LispString,
    restore_point: Option<(usize, usize)>,
) -> Result<(), crate::emacs_core::error::Flow> {
    ctx.buffers
        .insert_lisp_string_into_buffer(buffer_id, text)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    if let Some(table) = (!text.intervals().is_empty()).then(|| text.intervals()) {
        let byte_offset = restore_point
            .map(|(pt_byte, _)| pt_byte)
            .unwrap_or_else(|| {
                ctx.buffers
                    .get(buffer_id)
                    .map(|buf| buf.pt_byte.saturating_sub(text.sbytes()))
                    .unwrap_or(0)
            });
        ctx.buffers
            .append_buffer_text_properties(buffer_id, table, byte_offset)
            .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    }
    if let Some((pt_byte, pt)) = restore_point
        && let Some(buf) = ctx.buffers.get_mut(buffer_id)
    {
        buf.pt_byte = pt_byte;
        buf.pt = pt;
    }
    Ok(())
}

fn coding_string_destination(
    arg: Option<Value>,
) -> Result<Option<crate::buffer::BufferId>, crate::emacs_core::error::Flow> {
    let Some(value) = arg else {
        return Ok(None);
    };
    if value.is_nil() || value.is_t() {
        return Ok(None);
    }
    value
        .as_buffer_id()
        .map(Some)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("bufferp"), value]))
}

fn builtin_coding_string_in_context(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
    encode: bool,
) -> EvalResult {
    let name = if encode {
        "encode-coding-string"
    } else {
        "decode-coding-string"
    };
    expect_min_args(name, &args, 2)?;
    if args.len() > 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ));
    }
    let _ = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    let coding = context_coding_name(ctx, args[1])?;
    let destination = coding_string_destination(args.get(3).copied())?;
    let result = if encode {
        builtin_encode_coding_string_with_known(args, |_| true)?
    } else {
        builtin_decode_coding_string_with_known(args, |_| true)?
    };
    let result_text = result
        .as_lisp_string()
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), result],
            )
        })?
        .clone();
    ctx.set_variable(
        "last-coding-system-used",
        Value::symbol(&canonical_context_coding_name(ctx, &coding)),
    );

    let Some(buffer_id) = destination else {
        return Ok(result);
    };
    let restore_point = ctx.buffers.get(buffer_id).map(|buf| (buf.pt_byte, buf.pt));
    if restore_point.is_none() {
        return Err(signal(
            "error",
            vec![Value::string("Selecting deleted buffer")],
        ));
    }
    insert_coding_result(ctx, buffer_id, &result_text, restore_point)?;
    Ok(Value::fixnum(result_text.schars() as i64))
}

pub(crate) fn builtin_encode_coding_string_in_context(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_coding_string_in_context(ctx, args, true)
}

pub(crate) fn builtin_decode_coding_string_in_context(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_coding_string_in_context(ctx, args, false)
}

fn builtin_coding_region(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
    encode: bool,
) -> EvalResult {
    let name = if encode {
        "encode-coding-region"
    } else {
        "decode-coding-region"
    };
    expect_range_args(name, &args, 3, 4)?;

    let coding = context_coding_name(ctx, args[2])?;
    let destination = coding_region_destination(args.get(3).copied())?;
    let Some((start_byte, end_byte)) =
        crate::emacs_core::editfns::current_buffer_accessible_char_region_in_buffers(
            &ctx.buffers,
            &args[0],
            &args[1],
        )?
    else {
        return Ok(Value::NIL);
    };

    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(Value::NIL);
    };
    let source = ctx
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .buffer_substring_lisp_string(start_byte, end_byte);
    let result = transformed_region_string(source, &coding, encode)?;
    let result_text = result
        .as_lisp_string()
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), result],
            )
        })?
        .clone();
    let produced_chars = result_text.schars();

    match destination {
        None => {
            ctx.set_variable(
                "last-coding-system-used",
                Value::symbol(&canonical_context_coding_name(ctx, &coding)),
            );
            Ok(result)
        }
        Some(None) => {
            crate::emacs_core::editfns::ensure_current_buffer_writable_in_state(
                &ctx.obarray,
                &[],
                &ctx.buffers,
            )?;
            crate::emacs_core::fns::replace_buffer_region_lisp_string(
                ctx,
                current_id,
                start_byte,
                end_byte,
                &result_text,
            )?;
            if !result_text.intervals().is_empty() {
                ctx.buffers
                    .append_buffer_text_properties(current_id, result_text.intervals(), start_byte)
                    .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
            }
            ctx.set_variable(
                "last-coding-system-used",
                Value::symbol(&canonical_context_coding_name(ctx, &coding)),
            );
            Ok(Value::fixnum(produced_chars as i64))
        }
        Some(Some(buffer_id)) => {
            let restore_point = ctx.buffers.get(buffer_id).map(|buf| (buf.pt_byte, buf.pt));
            if restore_point.is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ));
            }
            insert_coding_result(ctx, buffer_id, &result_text, restore_point)?;
            ctx.set_variable(
                "last-coding-system-used",
                Value::symbol(&canonical_context_coding_name(ctx, &coding)),
            );
            Ok(Value::fixnum(produced_chars as i64))
        }
    }
}

pub(crate) fn builtin_encode_coding_region(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_coding_region(ctx, args, true)
}

pub(crate) fn builtin_decode_coding_region(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_coding_region(ctx, args, false)
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
    builtin_encode_coding_string_with_known(args, known_coding_system)
}

pub(crate) fn builtin_encode_coding_string_with_known(
    args: Vec<Value>,
    known: impl FnOnce(&str) -> bool,
) -> EvalResult {
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
        ValueKind::Nil => {
            return if coding_string_nocopy(&args) {
                Ok(args[0])
            } else {
                copy_lisp_string_value(args[0])
            };
        }
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };
    validate_coding_system(&coding, args[1], known)?;
    let bytes = encode_lisp_string(string, &coding);
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_unibyte(bytes),
    ))
}

/// `(decode-coding-string STRING CODING-SYSTEM)` -> string
pub(crate) fn builtin_decode_coding_string(args: Vec<Value>) -> EvalResult {
    builtin_decode_coding_string_with_known(args, known_coding_system)
}

pub(crate) fn builtin_decode_coding_string_with_known(
    args: Vec<Value>,
    known: impl FnOnce(&str) -> bool,
) -> EvalResult {
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
        ValueKind::Nil => {
            return if coding_string_nocopy(&args) {
                Ok(args[0])
            } else {
                copy_lisp_string_value(args[0])
            };
        }
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };
    validate_coding_system(&coding, args[1], known)?;
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
    let charset = match coding_system_family(&coding) {
        "iso-latin-1" => Some("iso-8859-1"),
        "chinese-iso-8bit" => Some("chinese-gb2312"),
        "chinese-big5" | "chinese-big5-hkscs" => Some("big5"),
        _ => None,
    };
    if let Some(charset) = charset {
        let runs = charset_property_runs(&decoded, charset);
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
