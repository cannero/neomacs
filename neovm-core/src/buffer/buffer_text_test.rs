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
