use super::BufferText;

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

    for &cp in &[0usize, 1, 50, 500, 5000, 12345, text.char_count() - 1, text.char_count()] {
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
