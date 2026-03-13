use super::*;
use crate::buffer::{Buffer, BufferId};

// -----------------------------------------------------------------------
// translate_emacs_regex
// -----------------------------------------------------------------------

#[test]
fn translate_groups() {
    // Emacs \( \) → Rust ( )
    assert_eq!(translate_emacs_regex("\\(foo\\)"), "(foo)");
}

#[test]
fn translate_alternation() {
    // Emacs \| → Rust |
    assert_eq!(translate_emacs_regex("foo\\|bar"), "foo|bar");
}

#[test]
fn translate_literal_parens() {
    // Emacs literal ( ) → Rust \( \)
    assert_eq!(translate_emacs_regex("(foo)"), "\\(foo\\)");
}

#[test]
fn translate_literal_braces() {
    // Emacs literal { } → Rust \{ \}
    assert_eq!(translate_emacs_regex("{3}"), "\\{3\\}");
}

#[test]
fn translate_repetition_braces() {
    // Emacs \{3\} → Rust {3}
    assert_eq!(translate_emacs_regex("a\\{3\\}"), "a{3}");
}

#[test]
fn translate_literal_pipe() {
    // Emacs literal | → Rust \|
    assert_eq!(translate_emacs_regex("a|b"), "a\\|b");
}

#[test]
fn translate_word_boundary() {
    // Emacs \< \> → Rust \b
    assert_eq!(translate_emacs_regex("\\<word\\>"), "\\bword\\b");
}

#[test]
fn translate_buffer_boundaries() {
    // Emacs \` → Rust \A, Emacs \' → Rust \z
    assert_eq!(translate_emacs_regex("\\`foo\\'"), "\\Afoo\\z");
}

#[test]
fn translate_character_class_passthrough() {
    // Character classes should pass through mostly unchanged
    assert_eq!(translate_emacs_regex("[a-z]"), "[a-z]");
    assert_eq!(translate_emacs_regex("[^0-9]"), "[^0-9]");
}

#[test]
fn translate_character_class_backslash_ranges_like_gnu() {
    assert_eq!(translate_emacs_regex("[+\\-*/=<>]"), "[+/=<>]");
}

#[test]
fn translate_easymenu_command_hint_regexp() {
    let emacs = r"^[^\]*\(\\\[\([^]]+\)]\)[^\]*$";
    assert_eq!(
        translate_emacs_regex(emacs),
        r"^[^\\]*(\\\[([^\]]+)])[^\\]*$"
    );
    compile_emacs_regex_case_fold(emacs, true).expect("easymenu regexp should compile");
}

#[test]
fn replace_match_case_capitalizes_each_word_like_gnu() {
    assert_eq!(apply_match_case("[alice:5]", "Alice"), "[Alice:5]");
    assert_eq!(
        apply_match_case("h_hello w_world", "Hello World"),
        "H_Hello W_World"
    );
}

#[test]
fn replace_match_case_upcases_all_caps_matches() {
    assert_eq!(apply_match_case("foo-bar", "FOO"), "FOO-BAR");
}

#[test]
fn translate_reversed_range_classes() {
    // Reversed ranges are empty in Emacs.
    assert_eq!(translate_emacs_regex("[z-a]"), "[^\\s\\S]");
    assert_eq!(translate_emacs_regex("[^z-a]"), "[\\s\\S]");
}

#[test]
fn translate_backslash_w() {
    assert_eq!(translate_emacs_regex("\\w+"), "\\w+");
}

#[test]
fn compile_search_pattern_uses_backref_engine_for_supported_captures() {
    assert!(matches!(
        compile_search_pattern("\\([a-z]+\\)-\\([0-9]+\\)", false),
        Ok(CompiledSearchPattern::Backref(_))
    ));
}

#[test]
fn compile_search_pattern_uses_backref_engine_for_noncapturing_groups() {
    assert!(matches!(
        compile_search_pattern("\\(?:foo\\|bar\\)+", false),
        Ok(CompiledSearchPattern::Backref(_))
    ));
}

#[test]
fn compile_search_pattern_routes_syntax_classes_through_backref_engine() {
    assert!(matches!(
        compile_search_pattern("\\(defun\\|defvar\\)\\s-+\\(\\w+\\)", false),
        Ok(CompiledSearchPattern::Backref(_))
    ));
}

#[test]
fn compile_search_pattern_routes_category_classes_through_backref_engine() {
    assert!(matches!(
        compile_search_pattern("[ \t]\\|\\c|.\\|.\\c|", false),
        Ok(CompiledSearchPattern::Backref(_))
    ));
}

#[test]
fn compile_search_pattern_keeps_digit_classes_on_regex_fallback() {
    assert!(matches!(
        compile_search_pattern("\\d+", false),
        Ok(CompiledSearchPattern::Regex(_))
    ));
}

#[test]
fn string_match_supported_capture_pattern_uses_backref_engine_semantics() {
    let mut md = None;
    let result =
        string_match_full_with_case_fold("\\([a-z]+\\)-\\([0-9]+\\)", "foo-123", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 7)));
    assert_eq!(md.groups[1], Some((0, 3)));
    assert_eq!(md.groups[2], Some((4, 7)));
}

#[test]
fn string_match_noncapturing_group_pattern_uses_backref_engine_semantics() {
    let mut md = None;
    let result =
        string_match_full_with_case_fold("\\(?:foo\\|bar\\)+", "foobar", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 6)));
    assert_eq!(md.groups.len(), 1);
}

#[test]
fn string_match_syntax_class_pattern_uses_backref_engine_semantics() {
    let mut md = None;
    let result = string_match_full_with_case_fold(
        "\\(defun\\|defvar\\)\\s-+\\(\\w+\\)",
        "defvar foo",
        0,
        false,
        &mut md,
    );
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 10)));
    assert_eq!(md.groups[1], Some((0, 6)));
    assert_eq!(md.groups[2], Some((7, 10)));
}

#[test]
fn string_match_word_syntax_class_pattern_uses_backref_engine_semantics() {
    let mut md = None;
    let result = string_match_full_with_case_fold("\\sw+", "foo_bar", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 7)));
}

#[test]
fn string_match_category_escape_pattern_uses_backref_engine_semantics() {
    let mut md = None;
    let result = string_match_full_with_case_fold("\\c|.", "éx", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 2)));
}

#[test]
fn string_match_match_at_point_escape_uses_backref_engine_semantics() {
    let mut md = None;
    let result = string_match_full_with_case_fold("\\=foo", "foo", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn translate_complex_pattern() {
    // Emacs: \(defun\|defvar\)\s-+\(\w+\)
    // Rust:  (defun|defvar)\s+(\w+)
    let emacs = "\\(defun\\|defvar\\)\\s-+\\(\\w+\\)";
    let rust = translate_emacs_regex(emacs);
    // After translation: (defun|defvar)\s+(\w+)
    assert_eq!(rust, "(defun|defvar)\\s+(\\w+)");
}

#[test]
fn translate_category_escape_keeps_fill_patterns_compilable() {
    let emacs = "[ \t]\\|\\c|.\\|.\\c|";
    let rust = translate_emacs_regex(emacs);
    assert_eq!(rust, "[ \t]|[^\\x00-\\x7F].|.[^\\x00-\\x7F]");
    compile_emacs_regex_case_fold(emacs, true).expect("fill category regexp should compile");
}

#[test]
fn translate_empty_pattern() {
    assert_eq!(translate_emacs_regex(""), "");
}

#[test]
fn translate_no_special_chars() {
    assert_eq!(translate_emacs_regex("hello"), "hello");
}

#[test]
fn translate_escaped_backslash() {
    assert_eq!(translate_emacs_regex("\\\\"), "\\\\");
}

#[test]
fn translate_multibyte_literals() {
    assert_eq!(translate_emacs_regex("\\(é\\)"), "(é)");
    assert_eq!(translate_emacs_regex("[éx]"), "[éx]");
    assert_eq!(translate_emacs_regex("\\é"), "é");
    assert_eq!(translate_emacs_regex("\\😀"), "😀");
}

#[test]
fn trivial_regexp_matches_gnu_meta_rules() {
    assert!(trivial_regexp_p("hello\\.txt"));
    assert!(trivial_regexp_p("\\😀"));
    assert!(!trivial_regexp_p("he.*o"));
    assert!(!trivial_regexp_p("\\(group\\)"));
    assert!(!trivial_regexp_p("\\1"));
    assert!(!trivial_regexp_p("trailing\\"));
}

#[test]
fn parse_segmented_template_patterns() {
    let interpolation = parse_segmented_pattern(r"{{\([^}]+\)}}").expect("interpolation");
    assert_eq!(interpolation.capture_count, 1);

    let foreach = parse_segmented_pattern(
        r"{%foreach \([^ ]+\) in \([^%]+\)%}\(\(?:.\|\n\)*?\){%endforeach%}",
    )
    .expect("foreach");
    assert_eq!(foreach.capture_count, 3);

    let conditional = parse_segmented_pattern(
        r"{%if \([^%]+\)%}\(\(?:.\|\n\)*?\){%else%}\(\(?:.\|\n\)*?\){%endif%}",
    )
    .expect("conditional");
    assert_eq!(conditional.capture_count, 3);
}

// -----------------------------------------------------------------------
// string_match_full
// -----------------------------------------------------------------------

#[test]
fn string_match_basic() {
    let mut md = None;
    let result = string_match_full("he..o", "hello world", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(0));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 5)));
    assert_eq!(md.searched_string_text(), Some("hello world".to_string()));
}

#[test]
fn string_match_with_groups() {
    let mut md = None;
    // Emacs regex: \(\w+\)@\(\w+\)
    let result = string_match_full("\\(\\w+\\)@\\(\\w+\\)", "user@host", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(0));
    let md = md.unwrap();
    assert_eq!(md.groups.len(), 3); // full + 2 groups
    assert_eq!(md.groups[0], Some((0, 9)));
    assert_eq!(md.groups[1], Some((0, 4))); // "user"
    assert_eq!(md.groups[2], Some((5, 9))); // "host"
}

#[test]
fn string_match_with_multibyte_group_literal() {
    let mut md = None;
    let result = string_match_full("\\(é\\)", "aéx", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((1, 2))); // "é" in character positions
    assert_eq!(md.groups[1], Some((1, 2))); // capture group
}

#[test]
fn string_match_with_escaped_multibyte_literal() {
    let mut md = None;
    let result = string_match_full("\\é", "aéx", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
}

#[test]
fn string_match_trivial_escaped_literal_uses_character_positions() {
    let mut md = None;
    let result = string_match_full("\\.", "a.b", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((1, 2)));
}

#[test]
fn string_match_backreference_reuses_captured_text() {
    let mut md = None;
    let result = string_match_full("\\(..\\)\\1", "zzabab", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(2));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((2, 6)));
    assert_eq!(md.groups[1], Some((2, 4)));
}

#[test]
fn looking_at_string_backreference_matches_at_start() {
    let mut md = None;
    let matched = looking_at_string("\\(x\\)\\1\\1", "xxx!", false, &mut md).unwrap();
    assert!(matched);
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 3)));
    assert_eq!(md.groups[1], Some((0, 1)));
}

#[test]
fn re_search_forward_backreference_word_boundary() {
    let mut buf = make_test_buffer("the the cat");
    let mut md = None;
    let result = re_search_forward(
        &mut buf,
        "\\b\\(\\w+\\) \\1\\b",
        None,
        false,
        false,
        &mut md,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(7));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 7)));
    assert_eq!(md.groups[1], Some((0, 3)));
}

#[test]
fn string_match_backreference_with_char_class_group() {
    let mut md = None;
    let result = string_match_full("\\([a-z]+\\) \\1", "the the cat", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(0));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 7)));
    assert_eq!(md.groups[1], Some((0, 3)));
}

#[test]
fn string_match_template_interpolation_pattern() {
    let mut md = None;
    let result = string_match_full(r"{{\([^}]+\)}}", "x {{name}} y", 0, &mut md).unwrap();
    assert_eq!(result, Some(2));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((2, 10)));
    assert_eq!(md.groups[1], Some((4, 8)));
}

#[test]
fn string_match_template_foreach_pattern() {
    let mut md = None;
    let result = string_match_full(
        r"{%foreach \([^ ]+\) in \([^%]+\)%}\(\(?:.\|\n\)*?\){%endforeach%}",
        "Items: {%foreach x in items%}[{{x}}] {%endforeach%}",
        0,
        &mut md,
    )
    .unwrap();
    assert_eq!(result, Some(7));
    let md = md.unwrap();
    assert_eq!(md.groups[1], Some((17, 18)));
    assert_eq!(md.groups[2], Some((22, 27)));
    assert_eq!(md.groups[3], Some((29, 37)));
}

#[test]
fn string_match_template_conditional_pattern() {
    let mut md = None;
    let result = string_match_full(
        r"{%if \([^%]+\)%}\(\(?:.\|\n\)*?\){%else%}\(\(?:.\|\n\)*?\){%endif%}",
        "{%if admin%}[ADMIN]{%else%}[USER]{%endif%}",
        0,
        &mut md,
    )
    .unwrap();
    assert_eq!(result, Some(0));
    let md = md.unwrap();
    assert_eq!(md.groups[1], Some((5, 10)));
    assert_eq!(md.groups[2], Some((12, 19)));
    assert_eq!(md.groups[3], Some((27, 33)));
}

#[test]
fn string_match_with_start_offset() {
    let mut md = None;
    let result = string_match_full("world", "hello world", 6, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(6));
}

#[test]
fn string_match_no_match() {
    let mut md = None;
    let result = string_match_full("xyz", "hello world", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
    assert!(md.is_none());
}

#[test]
fn string_match_emacs_alternation() {
    let mut md = None;
    // Emacs regex: \(foo\|bar\)
    let result = string_match_full("\\(foo\\|bar\\)", "test bar baz", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(5));
    let md = md.unwrap();
    assert_eq!(md.groups[1], Some((5, 8))); // "bar"
}

// -----------------------------------------------------------------------
// Buffer search: search_forward
// -----------------------------------------------------------------------

fn make_test_buffer(text: &str) -> Buffer {
    let mut buf = Buffer::new(BufferId(1), "test".to_string());
    buf.insert(text);
    // Reset point to beginning
    buf.pt = 0;
    // zv was updated by insert
    buf
}

#[test]
fn search_forward_basic() {
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let result = search_forward(&mut buf, "world", None, false, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(11)); // end of "world"
    assert_eq!(buf.pt, 11);
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((6, 11)));
}

#[test]
fn search_forward_not_found_noerror() {
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let result = search_forward(&mut buf, "xyz", None, true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
    assert_eq!(buf.pt, 0); // point unchanged
}

#[test]
fn search_forward_not_found_error() {
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let result = search_forward(&mut buf, "xyz", None, false, false, &mut md);
    assert!(result.is_err());
}

#[test]
fn search_forward_case_fold_true() {
    let mut buf = make_test_buffer("A");
    let mut md = None;
    let result = search_forward(&mut buf, "a", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
}

#[test]
fn search_forward_case_fold_true_unicode_literal() {
    let mut buf = make_test_buffer("Äx");
    let mut md = None;
    let result = search_forward(&mut buf, "ä", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some('Ä'.len_utf8()));
}

#[test]
fn re_search_forward_trivial_regexp_follows_literal_case_fold_path() {
    let mut buf = make_test_buffer("A.b");
    let mut md = None;
    let result = re_search_forward(&mut buf, "a\\.", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(2));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 2)));
}

#[test]
fn search_forward_with_bound() {
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    // Search only within first 5 bytes — "world" starts at 6 so should not be found
    let result = search_forward(&mut buf, "world", Some(5), true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[test]
fn search_forward_from_middle() {
    let mut buf = make_test_buffer("aaa bbb aaa");
    buf.pt = 4; // after "aaa "
    let mut md = None;
    let result = search_forward(&mut buf, "aaa", None, false, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(11)); // second "aaa" at end
}

// -----------------------------------------------------------------------
// Buffer search: search_backward
// -----------------------------------------------------------------------

#[test]
fn search_backward_basic() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 11; // end of buffer
    let mut md = None;
    let result = search_backward(&mut buf, "hello", None, false, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(0)); // beginning of "hello"
    assert_eq!(buf.pt, 0);
}

#[test]
fn search_backward_not_found() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 11;
    let mut md = None;
    let result = search_backward(&mut buf, "xyz", None, true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[test]
fn search_backward_finds_last_occurrence() {
    let mut buf = make_test_buffer("aaa bbb aaa");
    buf.pt = 11; // end
    let mut md = None;
    let result = search_backward(&mut buf, "aaa", None, false, false, &mut md);
    assert!(result.is_ok());
    // Should find the LAST "aaa" (at position 8)
    assert_eq!(result.unwrap(), Some(8));
    assert_eq!(buf.pt, 8);
}

#[test]
fn search_backward_case_fold_true_unicode_literal() {
    let mut buf = make_test_buffer("Ää");
    buf.pt = "Ää".len();
    let mut md = None;
    let result = search_backward(&mut buf, "ä", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some('Ä'.len_utf8()));
    assert_eq!(buf.pt, 'Ä'.len_utf8());
}

// -----------------------------------------------------------------------
// Buffer search: re_search_forward
// -----------------------------------------------------------------------

#[test]
fn re_search_forward_basic() {
    let mut buf = make_test_buffer("foo 123 bar");
    let mut md = None;
    let result = re_search_forward(&mut buf, "[0-9]+", None, false, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(7)); // end of "123"
    assert_eq!(buf.pt, 7);
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((4, 7)));
}

#[test]
fn re_search_forward_with_groups() {
    let mut buf = make_test_buffer("name: John");
    let mut md = None;
    // Emacs regex: \(\w+\): \(\w+\)
    let result = re_search_forward(
        &mut buf,
        "\\(\\w+\\): \\(\\w+\\)",
        None,
        false,
        false,
        &mut md,
    );
    assert!(result.is_ok());
    let md = md.unwrap();
    assert_eq!(md.groups.len(), 3);
    assert_eq!(md.groups[1], Some((0, 4))); // "name"
    assert_eq!(md.groups[2], Some((6, 10))); // "John"
}

#[test]
fn re_search_forward_multiline_anchor_respects_real_line_start() {
    let mut buf = make_test_buffer("alpha=1\nbeta=2\ngamma=3\n");
    let mut md = None;

    let first = re_search_forward(
        &mut buf,
        "^\\([^=]+\\)=\\([0-9]+\\)$",
        None,
        false,
        false,
        &mut md,
    )
    .expect("first search should succeed");
    assert_eq!(first, Some("alpha=1".len()));
    let first_md = md.as_ref().expect("match data for first search");
    let (s1, e1) = first_md.groups[1].unwrap();
    assert_eq!(buf.text.text_range(s1, e1), "alpha");

    let second = re_search_forward(
        &mut buf,
        "^\\([^=]+\\)=\\([0-9]+\\)$",
        None,
        false,
        false,
        &mut md,
    )
    .expect("second search should succeed");
    assert_eq!(second, Some("alpha=1\nbeta=2".len()));
    let second_md = md.as_ref().expect("match data for second search");
    let (s1, e1) = second_md.groups[1].unwrap();
    assert_eq!(buf.text.text_range(s1, e1), "beta");
    let (s2, e2) = second_md.groups[2].unwrap();
    assert_eq!(buf.text.text_range(s2, e2), "2");
}

// -----------------------------------------------------------------------
// Buffer search: re_search_backward
// -----------------------------------------------------------------------

#[test]
fn re_search_backward_basic() {
    let mut buf = make_test_buffer("abc 123 def 456");
    buf.pt = 15; // end
    let mut md = None;
    let result = re_search_backward(&mut buf, "[0-9]+", None, false, false, &mut md);
    assert!(result.is_ok());
    // Should find "456" (the last match)
    assert_eq!(result.unwrap(), Some(12));
    assert_eq!(buf.pt, 12);
}

// -----------------------------------------------------------------------
// looking_at
// -----------------------------------------------------------------------

#[test]
fn looking_at_matches() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 0;
    let mut md = None;
    let result = looking_at(&buf, "hello", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
    assert!(md.is_some());
}

#[test]
fn looking_at_no_match() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 0;
    let mut md = None;
    let result = looking_at(&buf, "world", true, &mut md);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn looking_at_from_middle() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 6; // "world"
    let mut md = None;
    let result = looking_at(&buf, "world", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn looking_at_defaults_to_case_fold() {
    let mut buf = make_test_buffer("A");
    buf.pt = 0;
    let mut md = None;
    let result = looking_at(&buf, "a", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn looking_at_respects_case_fold_false() {
    let mut buf = make_test_buffer("A");
    buf.pt = 0;
    let mut md = None;
    let result = looking_at(&buf, "a", false, &mut md);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn looking_at_with_groups() {
    let mut buf = make_test_buffer("foo123bar");
    buf.pt = 0;
    let mut md = None;
    // Emacs: \(\w+\)\([0-9]+\)
    let result = looking_at(&buf, "\\(\\w+\\)\\([0-9]+\\)", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
    let md = md.unwrap();
    // \w+ is greedy, matches "foo123bar" leaving nothing for [0-9]+
    // Actually \w includes digits, so \w+ matches everything
    // Let's check what actually happens
    assert!(md.groups[0].is_some());
}

#[test]
fn looking_at_character_class_backslash_range_like_gnu() {
    let mut md = None;
    let buf = make_test_buffer("/");
    let result = looking_at(&buf, "[+\\-*/=<>]", false, &mut md);
    assert_eq!(result, Ok(true));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 1)));

    let mut md = None;
    let buf = make_test_buffer("*");
    assert_eq!(looking_at(&buf, "[+\\-*/=<>]", false, &mut md), Ok(false));

    let mut md = None;
    let buf = make_test_buffer("-");
    assert_eq!(looking_at(&buf, "[+\\-*/=<>]", false, &mut md), Ok(false));
}

// -----------------------------------------------------------------------
// replace_match
// -----------------------------------------------------------------------

#[test]
fn replace_match_literal() {
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let _ = re_search_forward(&mut buf, "world", None, false, false, &mut md);
    let result = replace_match_buffer(&mut buf, "rust", false, true, 0, &md);
    assert!(result.is_ok());
    let content = buf.text.text_range(0, buf.text.len());
    assert_eq!(content, "hello rust");
}

#[test]
fn replace_match_with_backref() {
    let mut buf = make_test_buffer("hello world");
    buf.pt = 0;
    let mut md = None;
    // Match "hello" with a group
    let _ = re_search_forward(&mut buf, "\\(hello\\)", None, false, false, &mut md);
    let result = replace_match_buffer(&mut buf, "\\1 there", false, false, 0, &md);
    assert!(result.is_ok());
    let content = buf.text.text_range(0, buf.text.len());
    assert_eq!(content, "hello there world");
}

#[test]
fn replace_match_applies_case_pattern() {
    let mut md = None;
    let _ = string_match_full("FOO", "FOO", 0, &mut md);
    let replaced = replace_match_string("FOO", "bar", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "BAR");

    let _ = string_match_full("Foo", "Foo", 0, &mut md);
    let replaced = replace_match_string("Foo", "bar", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "Bar");
}

#[test]
fn replace_match_subexp_replaces_requested_group() {
    let mut md = None;
    let _ = string_match_full("\\([a-z]+\\)\\([0-9]+\\)", "abc123", 0, &mut md);
    let replaced = replace_match_string("abc123", "X", false, false, 2, &md).unwrap();
    assert_eq!(replaced, "abcX");
}

#[test]
fn replace_match_subexp_errors_when_missing() {
    let mut md = None;
    let _ = string_match_full("\\([a-z]+\\)?\\([0-9]+\\)", "123", 0, &mut md);
    let err = replace_match_string("123", "X", false, false, 1, &md).unwrap_err();
    assert_eq!(err, REPLACE_MATCH_SUBEXP_MISSING);
}

#[test]
fn replace_match_preserves_multibyte_replacement_literals() {
    let mut md = None;
    let _ = string_match_full("x", "x", 0, &mut md);
    let replaced = replace_match_string("x", "éz", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "éz");
}

#[test]
fn replace_match_preserves_multibyte_replacement_with_backref() {
    let mut md = None;
    let _ = string_match_full("\\(x\\)", "x", 0, &mut md);
    let replaced = replace_match_string("x", "\\1é", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "xé");
}

// -----------------------------------------------------------------------
// Integration: search + match data
// -----------------------------------------------------------------------

#[test]
fn search_forward_then_match_string() {
    let mut buf = make_test_buffer("The quick brown fox");
    let mut md = None;
    let _ = re_search_forward(
        &mut buf,
        "\\(quick\\) \\(brown\\)",
        None,
        false,
        false,
        &mut md,
    );
    let md = md.as_ref().unwrap();

    // match-string 0 = "quick brown"
    let (s0, e0) = md.groups[0].unwrap();
    assert_eq!(buf.text.text_range(s0, e0), "quick brown");

    // match-string 1 = "quick"
    let (s1, e1) = md.groups[1].unwrap();
    assert_eq!(buf.text.text_range(s1, e1), "quick");

    // match-string 2 = "brown"
    let (s2, e2) = md.groups[2].unwrap();
    assert_eq!(buf.text.text_range(s2, e2), "brown");
}

#[test]
fn string_match_then_match_data() {
    let mut md = None;
    let _ = string_match_full("\\([0-9]+\\)-\\([0-9]+\\)", "date: 2024-01-15", 0, &mut md);
    let md = md.as_ref().unwrap();
    let string = md.searched_string_text().unwrap();

    // match-beginning 0
    let (s0, _e0) = md.groups[0].unwrap();
    assert_eq!(s0, 6); // "2024-01"

    // Group 1: "2024"
    let (s1, e1) = md.groups[1].unwrap();
    assert_eq!(&string[s1..e1], "2024");

    // Group 2: "01"
    let (s2, e2) = md.groups[2].unwrap();
    assert_eq!(&string[s2..e2], "01");
}

#[test]
fn string_match_optional_group() {
    let mut md = None;
    // Pattern with an optional group: \(foo\)\(bar\)?
    let _ = string_match_full("\\(foo\\)\\(bar\\)?", "fooXYZ", 0, &mut md);
    let md = md.as_ref().unwrap();
    assert_eq!(md.groups[1], Some((0, 3))); // "foo"
    assert_eq!(md.groups[2], None); // optional group didn't match
}

#[test]
fn string_match_start_offset_respects_real_line_start() {
    let mut md = None;
    let source = "alpha=1\nbeta=2\ngamma=3";
    let start = "alpha=1".len();
    let result = string_match_full("^\\([^=]+\\)=\\([0-9]+\\)$", source, start, &mut md)
        .expect("string match should succeed");
    assert_eq!(result, Some("alpha=1\n".chars().count()));

    let md = md.as_ref().expect("match data");
    let searched = md.searched_string_text().expect("searched string");
    let (s1, e1) = md.groups[1].unwrap();
    let byte_s1 = searched
        .char_indices()
        .nth(s1)
        .map(|(i, _)| i)
        .unwrap_or(searched.len());
    let byte_e1 = searched
        .char_indices()
        .nth(e1)
        .map(|(i, _)| i)
        .unwrap_or(searched.len());
    assert_eq!(&searched[byte_s1..byte_e1], "beta");
}
