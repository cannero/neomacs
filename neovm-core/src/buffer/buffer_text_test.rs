use super::BufferText;

#[test]
fn from_lisp_string_preserves_unibyte_raw_bytes() {
    crate::test_utils::init_test_tracing();
    let raw = crate::heap_types::LispString::from_unibyte(vec![0xFF, b'A', 0x80]);
    let text = BufferText::from_lisp_string(&raw);

    assert!(!text.is_multibyte());
    assert_eq!(text.len(), 3);
    assert_eq!(text.char_count(), 3);

    let mut bytes = Vec::new();
    text.copy_emacs_bytes_to(0, text.emacs_byte_len(), &mut bytes);
    assert_eq!(bytes, vec![0xFF, b'A', 0x80]);
}

#[test]
fn char_count_tracks_multibyte_inserts_and_deletes() {
    crate::test_utils::init_test_tracing();
    let mut text = BufferText::from_str("ééz");
    assert_eq!(text.char_count(), 3);

    text.insert_str('é'.len_utf8(), "ß");
    assert_eq!(text.char_count(), 4);

    text.delete_range(2, 4);
    assert_eq!(text.char_count(), 3);
    assert_eq!(text.to_string(), "ééz");
}

#[test]
fn shared_clone_observes_cached_char_count_updates() {
    crate::test_utils::init_test_tracing();
    let mut text = BufferText::from_str("ab");
    let shared = text.shared_clone();
    text.insert_str(2, "é");
    assert_eq!(text.char_count(), 3);
    assert_eq!(shared.char_count(), 3);
}

#[test]
fn deep_clone_keeps_independent_char_count_cache() {
    crate::test_utils::init_test_tracing();
    let mut text = BufferText::from_str("ab");
    let cloned = text.clone();
    text.insert_str(2, "é");
    assert_eq!(text.char_count(), 3);
    assert_eq!(cloned.char_count(), 2);
}

#[test]
fn layout_tracks_gnu_style_gap_and_end_positions() {
    crate::test_utils::init_test_tracing();
    let mut text = BufferText::from_str("éz");
    let layout = text.layout();
    assert_eq!(layout.gpt, 2);
    assert_eq!(layout.z, 2);
    assert_eq!(layout.gpt_byte, 3);
    assert_eq!(layout.z_byte, 3);

    text.insert_str('é'.len_utf8(), "x");
    let layout = text.layout();
    assert_eq!(layout.gpt, 2);
    assert_eq!(layout.z, 3);
    assert_eq!(layout.gpt_byte, 3);
    assert_eq!(layout.z_byte, 4);
    assert_eq!(text.to_string(), "éxz");
}

#[test]
fn buf_charpos_to_bytepos_matches_oracle() {
    let mut s = String::new();
    for i in 0..5000 {
        if i % 2 == 0 {
            s.push_str("hello ");
        } else {
            s.push_str("日本語 ");
        }
    }
    let text = BufferText::from_str(&s);

    // Oracle: contiguous bytes → char_to_byte_pos.
    let mut bytes = Vec::new();
    text.copy_bytes_to(0, text.len(), &mut bytes);

    for &cp in &[
        0usize,
        1,
        50,
        500,
        5000,
        12345,
        text.char_count() - 1,
        text.char_count(),
    ] {
        let got = text.buf_charpos_to_bytepos(cp);
        let expected = crate::emacs_core::emacs_char::char_to_byte_pos(&bytes, cp);
        assert_eq!(
            got, expected,
            "charpos {cp}: buf_charpos_to_bytepos returned {got}, oracle said {expected}"
        );
    }
}

#[test]
fn buf_charpos_to_bytepos_invalidates_on_mutation() {
    let mut text = BufferText::from_str("abc");
    let first = text.buf_charpos_to_bytepos(2);
    assert_eq!(first, 2);

    // Insert "é" (2 bytes in UTF-8) at pos 0 — now charpos 2 sits at bytepos 3.
    text.insert_str(0, "é");
    let second = text.buf_charpos_to_bytepos(2);
    assert_eq!(second, 3);
    assert_ne!(first, second, "cache returned stale bytepos after mutation");
}

#[test]
fn buf_bytepos_to_charpos_matches_oracle() {
    let mut s = String::new();
    for i in 0..5000 {
        if i % 2 == 0 {
            s.push_str("hello ");
        } else {
            s.push_str("日本語 ");
        }
    }
    let text = BufferText::from_str(&s);

    let mut bytes = Vec::new();
    text.copy_bytes_to(0, text.len(), &mut bytes);

    for &bp in &[0usize, 1, 50, 500, 5000, 12345, text.len() - 1, text.len()] {
        // Oracle valid only on char boundaries — snap bp down to one.
        let mut bp_snapped = bp;
        while bp_snapped > 0 && bp_snapped < bytes.len() && (bytes[bp_snapped] & 0xC0) == 0x80 {
            bp_snapped -= 1;
        }
        let got = text.buf_bytepos_to_charpos(bp_snapped);
        let expected = crate::emacs_core::emacs_char::byte_to_char_pos(&bytes, bp_snapped);
        assert_eq!(got, expected, "bytepos {bp_snapped}");
    }
}

#[test]
fn long_scan_populates_anchor_cache() {
    // 20 000+ multibyte chars, no existing markers.
    // Query at the midpoint so the walk from either BEG or Z is >5000.
    let mut s = String::new();
    for _ in 0..20_000 {
        s.push_str("日");
    }
    let text = BufferText::from_str(&s);

    assert_eq!(text.anchor_cache_len(), 0);

    // 10 000 chars into a 20 000-char buffer — scan from nearest bracket
    // must walk 10 000 positions (> POSITION_ANCHOR_STRIDE=5000).
    let _ = text.buf_charpos_to_bytepos(10_000);

    assert!(
        text.anchor_cache_len() > 0,
        "expected auto-anchor to have been inserted after long scan (walked > 5000)"
    );
}

#[test]
fn replace_lisp_string_invalidates_position_cache() {
    crate::test_utils::init_test_tracing();
    // Build a buffer with a known multibyte char at charpos 2.
    let text = BufferText::from_str("日日日"); // 3 chars, 9 bytes
    let cached_before = text.buf_charpos_to_bytepos(2);
    assert_eq!(cached_before, 6);

    // Replace with different same-char-and-byte-count content.
    let lisp_string = crate::heap_types::LispString::from_utf8("本本本");
    text.replace_lisp_string(
        &lisp_string,
        crate::buffer::text_props::TextPropertyTable::new(),
    );

    // Same-count replacement would leave a stale pos_cache; verify it was
    // cleared by confirming the conversion is recomputed correctly. (The
    // byte position of charpos 2 must match the new content's layout.)
    let after = text.buf_charpos_to_bytepos(2);
    assert_eq!(after, 6, "charpos 2 in '本本本' is at bytepos 6");

    // Sanity: the actual bytes at that position are the lead byte of '本'.
    // '本' is 0xE6 0x9C 0xAC. So buffer[6] should be 0xE6.
    let b = text.byte_at(6);
    assert_eq!(
        b, 0xE6,
        "post-replace byte at position 6 should be 0xE6 (lead byte of 本)"
    );
}

#[test]
fn replace_lisp_string_handles_unibyte_raw_bytes() {
    crate::test_utils::init_test_tracing();
    let text = BufferText::from_str("ééz");
    let cached_before = text.buf_charpos_to_bytepos(2);
    assert_eq!(cached_before, 4);

    let raw = crate::heap_types::LispString::from_unibyte(vec![0xFF, b'A', 0x80]);
    text.replace_lisp_string(
        &raw,
        crate::buffer::text_props::TextPropertyTable::new(),
        Vec::new(),
    );

    assert!(!text.is_multibyte());
    assert_eq!(text.char_count(), 3);
    assert_eq!(text.buf_charpos_to_bytepos(2), 2);
    assert_eq!(text.byte_at(0), 0xFF);
    assert_eq!(text.byte_at(1), b'A');
    assert_eq!(text.byte_at(2), 0x80);

    let mut bytes = Vec::new();
    text.copy_emacs_bytes_to(0, text.emacs_byte_len(), &mut bytes);
    assert_eq!(bytes, vec![0xFF, b'A', 0x80]);
}
