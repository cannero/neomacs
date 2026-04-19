use super::*;

#[test]
fn test_simple_literal() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let result = search_pattern("hello", "say hello world", 0, false, &syn, 0);
    assert!(result.is_ok());
    let r = result.unwrap();
    assert!(r.is_some());
    let (pos, regs) = r.unwrap();
    assert_eq!(pos, 4); // "hello" starts at position 4
    assert_eq!(regs.end[0], 9); // ends at 9
}

#[test]
fn test_dot_matches_any() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let result = search_pattern("h.llo", "say hello world", 0, false, &syn, 0);
    assert!(result.is_ok());
    let r = result.unwrap();
    assert!(r.is_some());
}

#[test]
fn test_anchors() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // ^ at beginning
    let r = match_pattern("^hello", "hello world", 0, false, &syn, 0).unwrap();
    assert!(r.is_some());
    // ^ not at beginning
    let r = match_pattern("^hello", "say hello", 4, false, &syn, 0).unwrap();
    assert!(r.is_none());
}

#[test]
fn test_groups() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let result = search_pattern("\\(hel\\)lo", "hello", 0, false, &syn, 0);
    assert!(result.is_ok());
    let (pos, regs) = result.unwrap().unwrap();
    assert_eq!(pos, 0);
    assert_eq!(regs.start[1], 0); // group 1 start
    assert_eq!(regs.end[1], 3); // group 1 end ("hel")
}

#[test]
fn test_word_boundary() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("\\bhello\\b", "say hello world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_some());
}

#[test]
fn test_star_repetition() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("hel*o", "heo", 0, false, &syn, 0);
    assert!(r.unwrap().is_some()); // zero l's
    let r = search_pattern("hel*o", "hello", 0, false, &syn, 0);
    assert!(r.unwrap().is_some()); // two l's
    let r = search_pattern("hel*o", "hellllo", 0, false, &syn, 0);
    assert!(r.unwrap().is_some()); // four l's
}

#[test]
fn test_charset() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("[abc]", "xbz", 0, false, &syn, 0);
    assert!(r.unwrap().is_some());
    let r = search_pattern("[abc]", "xyz", 0, false, &syn, 0);
    assert!(r.unwrap().is_none());
}

#[test]
fn test_syntax_word() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // \sw matches word characters
    let r = search_pattern("\\sw+", "hello world", 0, false, &syn, 0);
    assert!(r.unwrap().is_some());
}

#[test]
fn test_backreference() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("\\(a\\)\\1", "aa", 0, false, &syn, 0);
    assert!(r.unwrap().is_some());
    let r = search_pattern("\\(a\\)\\1", "ab", 0, false, &syn, 0);
    assert!(r.unwrap().is_none());
}

#[test]
fn test_alternation() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("\\(foo\\|bar\\)", "test bar baz", 0, false, &syn, 0);
    assert!(r.is_ok(), "compile failed: {:?}", r.err());
    assert!(r.as_ref().unwrap().is_some(), "match failed");
    let (pos, regs) = r.unwrap().unwrap();
    assert_eq!(pos, 5, "match position");
    assert_eq!(regs.start[0], 5);
    assert_eq!(regs.end[0], 8);
}

#[test]
fn test_char_range() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("[0-9]+", "foo 123 bar", 0, false, &syn, 0);
    assert!(r.is_ok(), "compile failed: {:?}", r.err());
    assert!(r.as_ref().unwrap().is_some(), "match failed");
    let (pos, _regs) = r.unwrap().unwrap();
    assert_eq!(pos, 4, "match position");
}

#[test]
fn test_fastmap_skips_positions() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // Pattern starts with 'z' — should skip to position where 'z' appears
    let r = search_pattern("zing", "aaaaaaaaaazing", 0, false, &syn, 0);
    assert!(r.unwrap().is_some());
    let r = search_pattern("zing", "aaaaaaaaaazing", 0, false, &syn, 0);
    let (pos, _) = r.unwrap().unwrap();
    assert_eq!(pos, 10);
}

#[test]
fn test_fastmap_literal_accurate() {
    crate::test_utils::init_test_tracing();
    // Verify fastmap is populated and accurate for a simple literal
    let compiled = regex_compile("hello", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'h' as usize]);
    assert!(!compiled.fastmap[b'a' as usize]);
    assert!(!compiled.fastmap[b'z' as usize]);
}

#[test]
fn test_fastmap_charset() {
    crate::test_utils::init_test_tracing();
    // Verify fastmap for character class patterns
    let compiled = regex_compile("[abc]", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'a' as usize]);
    assert!(compiled.fastmap[b'b' as usize]);
    assert!(compiled.fastmap[b'c' as usize]);
    assert!(!compiled.fastmap[b'd' as usize]);
}

#[test]
fn test_fastmap_case_fold() {
    crate::test_utils::init_test_tracing();
    // Case-folded pattern should match both cases
    let compiled = regex_compile("Hello", false, true).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'h' as usize]);
    assert!(compiled.fastmap[b'H' as usize]);
}

#[test]
fn test_fastmap_alternation() {
    crate::test_utils::init_test_tracing();
    // Alternation: both branches should appear in fastmap
    let compiled = regex_compile("\\(foo\\|bar\\)", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'f' as usize]);
    assert!(compiled.fastmap[b'b' as usize]);
    assert!(!compiled.fastmap[b'z' as usize]);
}

#[test]
fn test_fastmap_dot() {
    crate::test_utils::init_test_tracing();
    // AnyChar: everything except newline
    let compiled = regex_compile(".", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'a' as usize]);
    assert!(compiled.fastmap[b'Z' as usize]);
    assert!(!compiled.fastmap[b'\n' as usize]);
}

#[test]
fn test_fastmap_anchor_then_literal() {
    crate::test_utils::init_test_tracing();
    // ^hello — anchor is zero-width, fastmap should see 'h'
    let compiled = regex_compile("^hello", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(compiled.fastmap[b'h' as usize]);
    assert!(!compiled.fastmap[b'x' as usize]);
}

#[test]
fn test_fastmap_charset_not() {
    crate::test_utils::init_test_tracing();
    // [^abc] should allow everything except a, b, c
    let compiled = regex_compile("[^abc]", false, false).unwrap();
    assert!(compiled.fastmap_accurate);
    assert!(!compiled.fastmap[b'a' as usize]);
    assert!(!compiled.fastmap[b'b' as usize]);
    assert!(!compiled.fastmap[b'c' as usize]);
    assert!(compiled.fastmap[b'd' as usize]);
    assert!(compiled.fastmap[b'z' as usize]);
}

#[test]
fn test_unterminated_charset_reports_gnu_ebrack() {
    crate::test_utils::init_test_tracing();
    match regex_compile("[invalid", false, false) {
        Ok(_) => panic!("unterminated charset should fail"),
        Err(err) => assert_eq!(err.message, "Unmatched [ or [^"),
    }
}

#[test]
fn test_multibyte_charset() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("[àáâ]", "hello à world", 0, false, &syn, 0);
    assert!(r.is_ok(), "compile failed: {:?}", r.err());
    assert!(r.unwrap().is_some(), "should match à in text");
}

#[test]
fn test_multibyte_charset_no_match() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("[àáâ]", "hello world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(
        r.unwrap().is_none(),
        "should not match when no accented chars"
    );
}

#[test]
fn test_multibyte_charset_range() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // Range of accented Latin characters: é (U+00E9) through ü (U+00FC)
    let r = search_pattern("[é-ü]", "hello ö world", 0, false, &syn, 0);
    assert!(r.is_ok(), "compile failed: {:?}", r.err());
    assert!(r.unwrap().is_some(), "ö should be in range é-ü");
}

#[test]
fn test_multibyte_charset_range_no_match() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // 'a' (U+0061) is outside the range é (U+00E9) through ü (U+00FC)
    let r = search_pattern("[é-ü]", "hello a world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_none(), "ASCII 'a' should not be in range é-ü");
}

#[test]
fn test_multibyte_charset_not() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // [^à] should match any character that is not à
    let r = search_pattern("[^à]", "à", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_none(), "[^à] should not match 'à'");

    let r = search_pattern("[^à]", "b", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_some(), "[^à] should match 'b'");
}

#[test]
fn test_multibyte_charset_mixed() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // Mix of ASCII and non-ASCII in one charset
    let r = search_pattern("[aéz]", "hello é world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_some(), "should match é");

    let r = search_pattern("[aéz]", "hello z world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_some(), "should also match z");
}

#[test]
fn test_multibyte_charset_cjk() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    // CJK characters
    let r = search_pattern("[你好世]", "say 好 to the world", 0, false, &syn, 0);
    assert!(r.is_ok());
    assert!(r.unwrap().is_some(), "should match 好");
}

#[test]
fn test_multibyte_charset_match_position() {
    crate::test_utils::init_test_tracing();
    let syn = DefaultSyntaxLookup;
    let r = search_pattern("[àáâ]", "hello á world", 0, false, &syn, 0);
    let (pos, regs) = r.unwrap().unwrap();
    assert_eq!(pos, 6, "á starts at byte 6");
    assert_eq!(regs.end[0], 8, "á is 2 bytes in UTF-8, ends at byte 8");
}
