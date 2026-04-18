use super::*;

#[test]
fn passes_through_control_chars_by_default() {
    crate::test_utils::init_test_tracing();
    // GNU Emacs default: only " and \ are escaped in prin1.
    // Control chars pass through literally (print-escape-newlines
    // and print-escape-control-characters are nil by default).
    assert_eq!(format_lisp_string("\n\t"), "\"\n\t\"");
    assert_eq!(format_lisp_string("\u{7f}"), "\"\u{7f}\"");
}

#[test]
fn keeps_non_bmp_visible() {
    crate::test_utils::init_test_tracing();
    assert_eq!(format_lisp_string("\u{10ffff}"), "\"\u{10ffff}\"");
}

#[test]
fn escapes_raw_byte_sentinel_as_octal() {
    crate::test_utils::init_test_tracing();
    let raw_377 = char::from_u32(0xE0FF).expect("valid sentinel scalar");
    assert_eq!(format_lisp_string(&raw_377.to_string()), "\"\\377\"");
}

#[test]
fn encode_nonunicode_char_uses_obsolete_utf8_bytes() {
    crate::test_utils::init_test_tracing();
    let encoded =
        encode_nonunicode_char_for_storage(0x110000).expect("non-unicode char should be encoded");
    assert_eq!(
        format_lisp_string_bytes(&encoded),
        vec![b'"', 0xF4, 0x90, 0x80, 0x80, b'"']
    );
}

#[test]
fn encode_nonunicode_char_uses_five_byte_sequence() {
    crate::test_utils::init_test_tracing();
    let encoded =
        encode_nonunicode_char_for_storage(0x200000).expect("non-unicode char should be encoded");
    assert_eq!(
        format_lisp_string_bytes(&encoded),
        vec![b'"', 0xF8, 0x88, 0x80, 0x80, 0x80, b'"']
    );
}

#[test]
fn decode_storage_char_codes_handles_nonunicode_and_raw_byte() {
    crate::test_utils::init_test_tracing();
    let encoded = format!(
        "{}{}",
        encode_nonunicode_char_for_storage(0x110000).expect("should encode"),
        encode_nonunicode_char_for_storage(0x3FFFFF).expect("raw byte should encode")
    );
    assert_eq!(
        decode_storage_char_codes(&encoded),
        vec![0x110000, 0x3FFFFF]
    );
}

#[test]
fn storage_char_len_for_nonunicode() {
    crate::test_utils::init_test_tracing();
    let ext = encode_nonunicode_char_for_storage(0x110000).expect("should encode");
    let raw = encode_nonunicode_char_for_storage(0x3FFFFF).expect("should encode");
    let s = format!("{ext}A{raw}");

    assert_eq!(storage_char_len(&s), 3);
}

#[test]
fn decode_storage_handles_overlong_raw_byte_encoding() {
    crate::test_utils::init_test_tracing();
    let encoded = emacs_bytes_to_storage_string(&[0xC1, 0xBF], true);
    assert_eq!(decode_storage_char_codes(&encoded), vec![0x3FFFFF]);
}

#[test]
fn unibyte_storage_string_round_trips_emacs_mule_bytes() {
    crate::test_utils::init_test_tracing();
    let encoded =
        bytes_to_unibyte_storage_string(&[0x06, b'"', b'\\', b'\n', 0x7F, 0x80, 0xA9, 0xFF]);
    assert_eq!(
        format_lisp_string_bytes(&encoded),
        vec![
            b'"', 0x06, b'\\', b'"', b'\\', b'\\', b'\n', 0x7F, b'\\', b'2', b'0', b'0', b'\\',
            b'2', b'5', b'1', b'\\', b'3', b'7', b'7', b'"'
        ]
    );
    assert_eq!(
        decode_storage_char_codes(&encoded),
        vec![6, 34, 92, 10, 127, 128, 169, 255]
    );
    assert_eq!(storage_char_len(&encoded), 8);
    assert_eq!(storage_byte_len(&encoded), 8);
}

#[test]
fn private_use_chars_outside_sentinel_ranges_are_not_rewritten() {
    crate::test_utils::init_test_tracing();
    let plain_private_use = char::from_u32(0xE400).expect("valid private-use scalar");
    let s = plain_private_use.to_string();
    assert_eq!(decode_storage_char_codes(&s), vec![0xE400]);
    assert_eq!(storage_char_len(&s), 1);
    assert_eq!(storage_byte_len(&s), s.len());
}
