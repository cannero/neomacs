use super::*;

// -----------------------------------------------------------------------
// Construction & basic queries
// -----------------------------------------------------------------------

#[test]
fn new_buffer_is_empty() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::new();
    assert_eq!(buf.len(), 0);
    assert!(buf.is_empty());
    assert_eq!(buf.char_count(), 0);
    assert_eq!(buf.to_string(), "");
}

#[test]
fn from_str_ascii() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello");
    assert_eq!(buf.len(), 5);
    assert_eq!(buf.char_count(), 5);
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn from_str_empty() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("");
    assert_eq!(buf.len(), 0);
    assert!(buf.is_empty());
    assert_eq!(buf.to_string(), "");
}

// -----------------------------------------------------------------------
// insert_str
// -----------------------------------------------------------------------

#[test]
fn insert_at_beginning() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("world");
    buf.insert_str(0, "hello ");
    assert_eq!(buf.to_string(), "hello world");
}

#[test]
fn insert_at_end() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.insert_str(5, " world");
    assert_eq!(buf.to_string(), "hello world");
}

#[test]
fn insert_in_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("helo");
    buf.insert_str(2, "l");
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn insert_into_empty_buffer() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::new();
    buf.insert_str(0, "abc");
    assert_eq!(buf.to_string(), "abc");
    assert_eq!(buf.len(), 3);
}

#[test]
fn insert_empty_string_is_noop() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.insert_str(3, "");
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn multiple_sequential_inserts() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::new();
    buf.insert_str(0, "a");
    buf.insert_str(1, "b");
    buf.insert_str(2, "c");
    buf.insert_str(3, "d");
    assert_eq!(buf.to_string(), "abcd");
}

#[test]
fn insert_larger_than_gap() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::new();
    let long = "x".repeat(256);
    buf.insert_str(0, &long);
    assert_eq!(buf.to_string(), long);
    assert_eq!(buf.len(), 256);
}

// -----------------------------------------------------------------------
// delete_range
// -----------------------------------------------------------------------

#[test]
fn delete_from_beginning() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello world");
    buf.delete_range(0, 6);
    assert_eq!(buf.to_string(), "world");
}

#[test]
fn delete_from_end() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello world");
    buf.delete_range(5, 11);
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn delete_from_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello world");
    buf.delete_range(5, 6); // delete the space
    assert_eq!(buf.to_string(), "helloworld");
}

#[test]
fn delete_everything() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.delete_range(0, 5);
    assert_eq!(buf.to_string(), "");
    assert!(buf.is_empty());
}

#[test]
fn delete_empty_range_is_noop() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.delete_range(2, 2);
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn delete_then_insert() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello world");
    buf.delete_range(5, 11);
    buf.insert_str(5, " rust");
    assert_eq!(buf.to_string(), "hello rust");
}

// -----------------------------------------------------------------------
// byte_at / char_at
// -----------------------------------------------------------------------

#[test]
fn byte_at_ascii() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("abcde");
    assert_eq!(buf.byte_at(0), b'a');
    assert_eq!(buf.byte_at(4), b'e');
}

#[test]
fn byte_at_after_gap_move() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("abcde");
    buf.move_gap_to(2);
    // Logical content unchanged.
    assert_eq!(buf.byte_at(0), b'a');
    assert_eq!(buf.byte_at(2), b'c');
    assert_eq!(buf.byte_at(4), b'e');
}

#[test]
fn char_at_ascii() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello");
    assert_eq!(buf.char_at(0), Some('h'));
    assert_eq!(buf.char_at(4), Some('o'));
    assert_eq!(buf.char_at(5), None);
}

#[test]
#[should_panic]
fn byte_at_out_of_range_panics() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hi");
    buf.byte_at(2);
}

// -----------------------------------------------------------------------
// text_range
// -----------------------------------------------------------------------

#[test]
fn text_range_full() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello world");
    assert_eq!(buf.text_range(0, 11), "hello world");
}

#[test]
fn text_range_prefix() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello world");
    assert_eq!(buf.text_range(0, 5), "hello");
}

#[test]
fn text_range_suffix() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello world");
    assert_eq!(buf.text_range(6, 11), "world");
}

#[test]
fn text_range_spanning_gap() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello world");
    buf.move_gap_to(5);
    // Range spans the gap.
    assert_eq!(buf.text_range(3, 8), "lo wo");
}

#[test]
fn text_range_empty() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello");
    assert_eq!(buf.text_range(2, 2), "");
}

// -----------------------------------------------------------------------
// move_gap_to
// -----------------------------------------------------------------------

#[test]
fn move_gap_to_start() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.move_gap_to(0);
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn move_gap_to_end() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.move_gap_to(5);
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn move_gap_around() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("abcdef");
    buf.move_gap_to(3);
    assert_eq!(buf.to_string(), "abcdef");
    buf.move_gap_to(0);
    assert_eq!(buf.to_string(), "abcdef");
    buf.move_gap_to(6);
    assert_eq!(buf.to_string(), "abcdef");
    buf.move_gap_to(2);
    assert_eq!(buf.to_string(), "abcdef");
}

// -----------------------------------------------------------------------
// ensure_gap
// -----------------------------------------------------------------------

#[test]
fn ensure_gap_grows() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    let old_gap = buf.gap_size();
    buf.ensure_gap(old_gap + 100);
    assert!(buf.gap_size() >= old_gap + 100);
    // Content must be preserved.
    assert_eq!(buf.to_string(), "hello");
}

#[test]
fn ensure_gap_noop_when_large_enough() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    let old_gap = buf.gap_size();
    buf.ensure_gap(1);
    assert_eq!(buf.gap_size(), old_gap);
}

// -----------------------------------------------------------------------
// Multibyte / UTF-8 (CJK, emoji)
// -----------------------------------------------------------------------

#[test]
fn multibyte_cjk() {
    crate::test_utils::init_test_tracing();
    // Each CJK character is 3 bytes in UTF-8.
    let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}"; // 你好世界
    let buf = GapBuffer::from_str(text);
    assert_eq!(buf.len(), 12); // 4 chars * 3 bytes
    assert_eq!(buf.char_count(), 4);
    assert_eq!(buf.to_string(), text);

    // char_at at byte boundaries
    assert_eq!(buf.char_at(0), Some('\u{4F60}')); // 你
    assert_eq!(buf.char_at(3), Some('\u{597D}')); // 好
    assert_eq!(buf.char_at(6), Some('\u{4E16}')); // 世
    assert_eq!(buf.char_at(9), Some('\u{754C}')); // 界
}

#[test]
fn multibyte_emoji() {
    crate::test_utils::init_test_tracing();
    // Emoji are 4 bytes in UTF-8.
    let text = "\u{1F600}\u{1F60D}"; // two emoji
    let buf = GapBuffer::from_str(text);
    assert_eq!(buf.len(), 8);
    assert_eq!(buf.char_count(), 2);
    assert_eq!(buf.char_at(0), Some('\u{1F600}'));
    assert_eq!(buf.char_at(4), Some('\u{1F60D}'));
}

#[test]
fn insert_multibyte_in_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("ab");
    buf.insert_str(1, "\u{1F600}"); // insert emoji between a and b
    assert_eq!(buf.to_string(), "a\u{1F600}b");
    assert_eq!(buf.len(), 6); // 1 + 4 + 1
    assert_eq!(buf.char_count(), 3);
}

#[test]
fn delete_multibyte_char() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("a\u{4F60}b"); // a你b
    // Delete the CJK char (bytes 1..4).
    buf.delete_range(1, 4);
    assert_eq!(buf.to_string(), "ab");
}

#[test]
fn text_range_multibyte_spanning_gap() {
    crate::test_utils::init_test_tracing();
    let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}"; // 你好世界
    let mut buf = GapBuffer::from_str(text);
    buf.move_gap_to(6); // gap between 好 and 世
    assert_eq!(buf.text_range(0, 6), "\u{4F60}\u{597D}");
    assert_eq!(buf.text_range(6, 12), "\u{4E16}\u{754C}");
    assert_eq!(buf.text_range(3, 9), "\u{597D}\u{4E16}");
}

#[test]
fn mixed_ascii_and_multibyte() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello\u{4E16}\u{754C}!");
    // "hello世界!" — 5 + 3 + 3 + 1 = 12 bytes, 8 chars
    assert_eq!(buf.len(), 12);
    assert_eq!(buf.char_count(), 8);

    buf.insert_str(5, " ");
    assert_eq!(buf.to_string(), "hello \u{4E16}\u{754C}!");
    assert_eq!(buf.len(), 13);

    buf.delete_range(6, 12); // delete "世界"
    assert_eq!(buf.to_string(), "hello !");
}

// -----------------------------------------------------------------------
// byte_to_char / char_to_byte
// -----------------------------------------------------------------------

#[test]
fn byte_char_roundtrip_ascii() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hello");
    for i in 0..=5 {
        assert_eq!(buf.byte_to_char(i), i);
        assert_eq!(buf.char_to_byte(i), i);
    }
}

#[test]
fn byte_to_char_cjk() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("\u{4F60}\u{597D}\u{4E16}"); // 你好世
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.byte_to_char(3), 1);
    assert_eq!(buf.byte_to_char(6), 2);
    assert_eq!(buf.byte_to_char(9), 3);
}

#[test]
fn char_to_byte_cjk() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("\u{4F60}\u{597D}\u{4E16}"); // 你好世
    assert_eq!(buf.char_to_byte(0), 0);
    assert_eq!(buf.char_to_byte(1), 3);
    assert_eq!(buf.char_to_byte(2), 6);
    assert_eq!(buf.char_to_byte(3), 9);
}

#[test]
fn byte_char_roundtrip_mixed() {
    crate::test_utils::init_test_tracing();
    // "a你b" — byte offsets: a=0, 你=1..4, b=4
    let buf = GapBuffer::from_str("a\u{4F60}b");
    assert_eq!(buf.byte_to_char(0), 0); // before 'a'
    assert_eq!(buf.byte_to_char(1), 1); // before '你'
    assert_eq!(buf.byte_to_char(4), 2); // before 'b'
    assert_eq!(buf.byte_to_char(5), 3); // end

    assert_eq!(buf.char_to_byte(0), 0);
    assert_eq!(buf.char_to_byte(1), 1);
    assert_eq!(buf.char_to_byte(2), 4);
    assert_eq!(buf.char_to_byte(3), 5);
}

#[test]
fn byte_char_conversion_with_gap_in_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("a\u{4F60}b\u{597D}c");
    // Move gap to middle of the text.
    buf.move_gap_to(4); // between 你 and b
    // Conversions should be unaffected by gap position.
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.byte_to_char(1), 1);
    assert_eq!(buf.byte_to_char(4), 2);
    assert_eq!(buf.byte_to_char(5), 3);
    assert_eq!(buf.byte_to_char(8), 4);

    assert_eq!(buf.char_to_byte(0), 0);
    assert_eq!(buf.char_to_byte(1), 1);
    assert_eq!(buf.char_to_byte(2), 4);
    assert_eq!(buf.char_to_byte(3), 5);
    assert_eq!(buf.char_to_byte(4), 8);
}

#[test]
fn byte_char_conversion_empty() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::new();
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.char_to_byte(0), 0);
}

#[test]
fn byte_to_char_emoji() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("x\u{1F600}y"); // x😀y
    // byte offsets: x=0, 😀=1..5, y=5
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.byte_to_char(1), 1);
    assert_eq!(buf.byte_to_char(5), 2);
    assert_eq!(buf.byte_to_char(6), 3);
}

#[test]
fn byte_char_conversion_unibyte_storage_sentinels() {
    crate::test_utils::init_test_tracing();
    let storage =
        crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0x80, b'A', 0xFF]);
    let buf = GapBuffer::from_str(&storage);
    assert_eq!(buf.char_count(), 3);
    assert_eq!(buf.char_to_byte(0), 0);
    assert_eq!(buf.char_to_byte(1), 1);
    assert_eq!(buf.char_to_byte(2), 2);
    assert_eq!(buf.char_to_byte(3), 3);
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.byte_to_char(1), 1);
    assert_eq!(buf.byte_to_char(2), 2);
    assert_eq!(buf.byte_to_char(3), 3);
}

#[test]
fn byte_char_conversion_unibyte_storage_sentinels_after_gap_move() {
    crate::test_utils::init_test_tracing();
    let storage =
        crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[0x80, b'A', 0xFF]);
    let mut buf = GapBuffer::from_str(&storage);
    buf.move_gap_to(2);
    assert_eq!(buf.char_count(), 3);
    assert_eq!(buf.char_to_byte(0), 0);
    assert_eq!(buf.char_to_byte(1), 1);
    assert_eq!(buf.char_to_byte(2), 2);
    assert_eq!(buf.char_to_byte(3), 3);
    assert_eq!(buf.byte_to_char(0), 0);
    assert_eq!(buf.byte_to_char(1), 1);
    assert_eq!(buf.byte_to_char(2), 2);
    assert_eq!(buf.byte_to_char(3), 3);
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn repeated_insert_delete_cycle() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::new();
    for i in 0..100 {
        let s = format!("{i}");
        buf.insert_str(buf.len(), &s);
    }
    let full = buf.to_string();
    assert!(!full.is_empty());

    // Delete everything one byte at a time from the front.
    while !buf.is_empty() {
        buf.delete_range(0, 1);
    }
    assert!(buf.is_empty());
    assert_eq!(buf.to_string(), "");
}

#[test]
fn gap_moves_correctly_after_multiple_operations() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("the quick brown fox");
    buf.delete_range(4, 10); // delete "quick "
    assert_eq!(buf.to_string(), "the brown fox");
    buf.insert_str(4, "slow ");
    assert_eq!(buf.to_string(), "the slow brown fox");
    buf.delete_range(9, 15); // delete "brown "
    assert_eq!(buf.to_string(), "the slow fox");
    buf.insert_str(9, "red ");
    assert_eq!(buf.to_string(), "the slow red fox");
}

#[test]
fn insert_at_every_position() {
    crate::test_utils::init_test_tracing();
    for pos in 0..=5 {
        let mut buf = GapBuffer::from_str("hello");
        buf.insert_str(pos, "X");
        assert_eq!(buf.len(), 6);
        assert_eq!(buf.byte_at(pos), b'X');
    }
}

#[test]
fn display_trait() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("display test");
    let s = format!("{buf}");
    assert_eq!(s, "display test");
}

#[test]
fn debug_trait_contains_text() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("dbg");
    let dbg = format!("{buf:?}");
    assert!(dbg.contains("dbg"));
    assert!(dbg.contains("GapBuffer"));
}

#[test]
fn default_is_empty() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::default();
    assert!(buf.is_empty());
}

#[test]
fn clone_is_independent() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("original");
    let clone = buf.clone();
    buf.insert_str(0, "X");
    assert_eq!(buf.to_string(), "Xoriginal");
    assert_eq!(clone.to_string(), "original");
}

#[test]
#[should_panic]
fn insert_past_end_panics() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hi");
    buf.insert_str(3, "x");
}

#[test]
#[should_panic]
fn delete_past_end_panics() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hi");
    buf.delete_range(0, 3);
}

#[test]
#[should_panic]
fn delete_inverted_range_panics() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hello");
    buf.delete_range(3, 1);
}

#[test]
#[should_panic]
fn text_range_past_end_panics() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hi");
    buf.text_range(0, 3);
}

#[test]
#[should_panic]
fn move_gap_past_end_panics() {
    crate::test_utils::init_test_tracing();
    let mut buf = GapBuffer::from_str("hi");
    buf.move_gap_to(3);
}

#[test]
#[should_panic]
fn byte_to_char_past_end_panics() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hi");
    buf.byte_to_char(3);
}

#[test]
fn char_to_byte_past_end_clamps() {
    crate::test_utils::init_test_tracing();
    let buf = GapBuffer::from_str("hi");
    // char_to_byte clamps to buffer end instead of panicking
    // when char_pos exceeds char_count (for stale positions).
    assert_eq!(buf.char_to_byte(3), buf.len());
    assert_eq!(buf.char_to_byte(100), buf.len());
}

// -----------------------------------------------------------------------
// copy_bytes_to
// -----------------------------------------------------------------------

#[test]
fn copy_bytes_to_basic() {
    crate::test_utils::init_test_tracing();
    let gb = GapBuffer::from_str("Hello, world!");
    let mut out = Vec::new();
    gb.copy_bytes_to(0, 5, &mut out);
    assert_eq!(&out, b"Hello");

    gb.copy_bytes_to(7, 13, &mut out);
    assert_eq!(&out, b"world!");
}

#[test]
fn copy_bytes_to_spanning_gap() {
    crate::test_utils::init_test_tracing();
    let mut gb = GapBuffer::from_str("abcdef");
    gb.move_gap_to(3); // gap after "abc"
    let mut out = Vec::new();
    gb.copy_bytes_to(1, 5, &mut out); // "bcde" — spans gap
    assert_eq!(&out, b"bcde");
}

#[test]
fn copy_bytes_to_empty_range() {
    crate::test_utils::init_test_tracing();
    let gb = GapBuffer::from_str("test");
    let mut out = vec![1, 2, 3]; // pre-existing contents
    gb.copy_bytes_to(2, 2, &mut out);
    assert!(out.is_empty());
}

#[test]
fn copy_emacs_bytes_to_unibyte_storage_sentinels() {
    crate::test_utils::init_test_tracing();
    let storage = crate::emacs_core::string_escape::bytes_to_unibyte_storage_string(&[
        0xFF, b'\n', 0x80, b'A',
    ]);
    let mut gb = GapBuffer::from_str(&storage);
    let gap_pos = gb.emacs_byte_to_storage_byte(2);
    gb.move_gap_to(gap_pos);

    let mut out = Vec::new();
    gb.copy_emacs_bytes_to(0, 4, &mut out);
    assert_eq!(out, vec![0xFF, b'\n', 0x80, b'A']);

    gb.copy_emacs_bytes_to(1, 3, &mut out);
    assert_eq!(out, vec![b'\n', 0x80]);
}

// -----------------------------------------------------------------------
// GNU parity tests (gap-sizing constants)
// -----------------------------------------------------------------------

#[test]
fn new_buffer_has_gnu_default_gap_size() {
    crate::test_utils::init_test_tracing();
    let gb = GapBuffer::new();
    assert!(
        gb.gap_size() >= 2000,
        "expected gap_size >= 2000, got {}",
        gb.gap_size()
    );
}

#[test]
fn ensure_gap_grows_beyond_requested_minimum() {
    crate::test_utils::init_test_tracing();
    let mut gb = GapBuffer::new();
    // Fill current gap completely so the next ensure_gap must actually grow.
    let filler = vec![b'a'; gb.gap_size()];
    gb.insert_emacs_bytes(0, &filler);
    assert_eq!(gb.gap_size(), 0);
    gb.ensure_gap(1);
    // GNU adds GAP_BYTES_DFL beyond caller's request.
    assert!(
        gb.gap_size() >= 2000,
        "expected ensure_gap(1) to grow gap to >= 2000, got {}",
        gb.gap_size()
    );
}
