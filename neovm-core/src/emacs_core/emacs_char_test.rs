use super::*;

// -----------------------------------------------------------------------
// ASCII roundtrip
// -----------------------------------------------------------------------

#[test]
fn ascii_roundtrip_all_128() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    for byte in 0u8..128 {
        let c = byte as u32;
        assert_eq!(char_bytes(c), 1);
        let n = char_string(c, &mut buf);
        assert_eq!(n, 1);
        assert_eq!(buf[0], byte);
        let (decoded, len) = string_char(&buf[..n]);
        assert_eq!(decoded, c);
        assert_eq!(len, 1);
    }
}

// -----------------------------------------------------------------------
// 2-byte Unicode roundtrip (U+00E9 = e-acute)
// -----------------------------------------------------------------------

#[test]
fn two_byte_unicode_roundtrip() {
    let c: u32 = 0xE9; // e-acute
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 2);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 2);
    // Standard UTF-8 for U+00E9: C3 A9
    assert_eq!(buf[0], 0xC3);
    assert_eq!(buf[1], 0xA9);
    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 2);
}

// -----------------------------------------------------------------------
// 3-byte Unicode roundtrip (U+2018 = left single quotation mark)
// -----------------------------------------------------------------------

#[test]
fn three_byte_unicode_roundtrip() {
    let c: u32 = 0x2018;
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 3);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 3);
    // UTF-8 for U+2018: E2 80 98
    assert_eq!(buf[0], 0xE2);
    assert_eq!(buf[1], 0x80);
    assert_eq!(buf[2], 0x98);
    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 3);
}

// -----------------------------------------------------------------------
// 4-byte Unicode roundtrip (U+1F344 = mushroom emoji)
// -----------------------------------------------------------------------

#[test]
fn four_byte_unicode_roundtrip() {
    let c: u32 = 0x1F344;
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 4);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 4);
    // UTF-8 for U+1F344: F0 9F 8D 84
    assert_eq!(buf[0], 0xF0);
    assert_eq!(buf[1], 0x9F);
    assert_eq!(buf[2], 0x8D);
    assert_eq!(buf[3], 0x84);
    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 4);
}

// -----------------------------------------------------------------------
// Raw byte 0x80 roundtrip
// -----------------------------------------------------------------------

#[test]
fn raw_byte_0x80_roundtrip() {
    let byte: u8 = 0x80;
    let c = byte8_to_char(byte);
    assert_eq!(c, 0x3FFF80);
    assert!(char_byte8_p(c));
    assert_eq!(char_to_byte8(c), byte);

    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 2);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 2);
    // Overlong encoding: C0 80
    assert_eq!(buf[0], 0xC0);
    assert_eq!(buf[1], 0x80);

    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 2);
}

// -----------------------------------------------------------------------
// Raw byte 0xFF roundtrip
// -----------------------------------------------------------------------

#[test]
fn raw_byte_0xff_roundtrip() {
    let byte: u8 = 0xFF;
    let c = byte8_to_char(byte);
    assert_eq!(c, 0x3FFFFF);
    assert!(char_byte8_p(c));
    assert_eq!(char_to_byte8(c), byte);

    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 2);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 2);
    // Overlong encoding: C1 BF
    assert_eq!(buf[0], 0xC1);
    assert_eq!(buf[1], 0xBF);

    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 2);
}

// -----------------------------------------------------------------------
// All raw bytes 0x80..0xFF roundtrip
// -----------------------------------------------------------------------

#[test]
fn raw_byte_all_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    for byte in 0x80u8..=0xFF {
        let c = byte8_to_char(byte);
        assert!(char_byte8_p(c));
        assert_eq!(char_to_byte8(c), byte);
        let n = char_string(c, &mut buf);
        assert_eq!(n, 2);
        let (decoded, len) = string_char(&buf[..n]);
        assert_eq!(decoded, c, "roundtrip failed for byte 0x{:02X}", byte);
        assert_eq!(len, 2);
    }
}

// -----------------------------------------------------------------------
// chars_in_multibyte
// -----------------------------------------------------------------------

#[test]
fn chars_in_multibyte_mixed() {
    // "Ae\u{0301}" (A, e-acute) + raw byte 0x80
    // A = 1 byte, e-acute = 2 bytes, raw 0x80 = 2 bytes → 5 bytes, 3 chars
    let mut data = Vec::new();
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];

    let n = char_string(b'A' as u32, &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(0xE9, &mut buf); // e-acute
    data.extend_from_slice(&buf[..n]);

    let n = char_string(byte8_to_char(0x80), &mut buf);
    data.extend_from_slice(&buf[..n]);

    assert_eq!(chars_in_multibyte(&data), 3);
    assert_eq!(data.len(), 5);
}

#[test]
fn chars_in_multibyte_empty() {
    assert_eq!(chars_in_multibyte(&[]), 0);
}

#[test]
fn chars_in_multibyte_ascii_only() {
    assert_eq!(chars_in_multibyte(b"hello"), 5);
}

// -----------------------------------------------------------------------
// char_to_byte_pos / byte_to_char_pos
// -----------------------------------------------------------------------

#[test]
fn char_byte_pos_conversion() {
    // Build: "A" (1 byte) + U+2018 (3 bytes) + raw 0xFF (2 bytes) + "B" (1 byte)
    // Char indices:  0       1                  2                    3
    // Byte offsets:  0       1                  4                    6
    let mut data = Vec::new();
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];

    let n = char_string(b'A' as u32, &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(0x2018, &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(byte8_to_char(0xFF), &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(b'B' as u32, &mut buf);
    data.extend_from_slice(&buf[..n]);

    assert_eq!(data.len(), 7); // 1 + 3 + 2 + 1

    // char_to_byte_pos
    assert_eq!(char_to_byte_pos(&data, 0), 0);
    assert_eq!(char_to_byte_pos(&data, 1), 1);
    assert_eq!(char_to_byte_pos(&data, 2), 4);
    assert_eq!(char_to_byte_pos(&data, 3), 6);
    assert_eq!(char_to_byte_pos(&data, 4), 7); // past end

    // byte_to_char_pos
    assert_eq!(byte_to_char_pos(&data, 0), 0);
    assert_eq!(byte_to_char_pos(&data, 1), 1);
    assert_eq!(byte_to_char_pos(&data, 4), 2);
    assert_eq!(byte_to_char_pos(&data, 6), 3);
    assert_eq!(byte_to_char_pos(&data, 7), 4);
}

// -----------------------------------------------------------------------
// try_as_utf8
// -----------------------------------------------------------------------

#[test]
fn try_as_utf8_valid() {
    let s = "hello world";
    let bytes = utf8_to_emacs(s);
    assert_eq!(try_as_utf8(&bytes), Some(s));
}

#[test]
fn try_as_utf8_with_unicode() {
    let s = "\u{2018}cafe\u{0301}\u{2019}";
    let bytes = utf8_to_emacs(s);
    assert_eq!(try_as_utf8(&bytes), Some(s));
}

#[test]
fn try_as_utf8_with_raw_bytes() {
    // Encode raw byte 0x80 — this produces overlong C0 80 which is not valid UTF-8.
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    let n = char_string(byte8_to_char(0x80), &mut buf);
    assert!(try_as_utf8(&buf[..n]).is_none());
}

// -----------------------------------------------------------------------
// to_utf8_lossy
// -----------------------------------------------------------------------

#[test]
fn to_utf8_lossy_clean() {
    let s = "hello";
    let bytes = utf8_to_emacs(s);
    assert_eq!(to_utf8_lossy(&bytes), "hello");
}

#[test]
fn to_utf8_lossy_with_raw_bytes() {
    // "A" + raw 0x80 + "B"
    let mut data = Vec::new();
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];

    let n = char_string(b'A' as u32, &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(byte8_to_char(0x80), &mut buf);
    data.extend_from_slice(&buf[..n]);

    let n = char_string(b'B' as u32, &mut buf);
    data.extend_from_slice(&buf[..n]);

    assert_eq!(to_utf8_lossy(&data), "A\u{FFFD}B");
}

// -----------------------------------------------------------------------
// utf8_to_emacs roundtrip
// -----------------------------------------------------------------------

#[test]
fn utf8_to_emacs_roundtrip() {
    let cases = [
        "",
        "hello",
        "\u{E9}",    // e-acute
        "\u{2018}",  // left single quote
        "\u{1F344}", // mushroom
        "mix\u{E9}d \u{2018}text\u{2019} with \u{1F344}",
    ];
    for s in cases {
        let emacs_bytes = utf8_to_emacs(s);
        // Since there are no raw bytes, try_as_utf8 should succeed and
        // return the original string.
        assert_eq!(
            try_as_utf8(&emacs_bytes),
            Some(s),
            "roundtrip failed for {:?}",
            s
        );
    }
}

// -----------------------------------------------------------------------
// 5-byte character (extended Emacs range)
// -----------------------------------------------------------------------

#[test]
fn five_byte_char_roundtrip() {
    // U+200000 is the start of the 5-byte range
    let c: u32 = 0x20_0000;
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 5);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 5);
    assert_eq!(buf[0], 0xF8);
    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 5);
}

#[test]
fn five_byte_max_roundtrip() {
    let c: u32 = MAX_5_BYTE_CHAR; // 0x3FFF7F
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    assert_eq!(char_bytes(c), 5);
    let n = char_string(c, &mut buf);
    assert_eq!(n, 5);
    let (decoded, len) = string_char(&buf[..n]);
    assert_eq!(decoded, c);
    assert_eq!(len, 5);
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn char_byte8_p_boundary() {
    assert!(!char_byte8_p(MAX_5_BYTE_CHAR));
    assert!(char_byte8_p(MAX_5_BYTE_CHAR + 1));
    assert!(char_byte8_p(MAX_CHAR));
}

#[test]
fn byte8_to_char_ascii_passthrough() {
    for b in 0u8..0x80 {
        assert_eq!(byte8_to_char(b), b as u32);
    }
}

#[test]
fn char_to_byte_pos_beyond_end() {
    let data = b"AB";
    assert_eq!(char_to_byte_pos(data, 5), 2); // clamps to len
}

#[test]
fn byte_to_char_pos_at_zero() {
    let data = b"hello";
    assert_eq!(byte_to_char_pos(data, 0), 0);
}
