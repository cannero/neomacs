use super::*;
use crate::buffer::{Buffer, BufferId};
use crate::emacs_core::value::Value;
use crate::heap_types::LispString;

fn extract_heap_match_string(md: &MatchData, group: usize) -> Option<String> {
    let searched = match md.searched_string.as_ref()? {
        SearchedString::Heap(val) => SearchedString::Heap(*val),
        SearchedString::Owned(text) => SearchedString::Owned(text.clone()),
    };
    let (start, end) = md.groups.get(group).and_then(|group| *group)?;
    let string = searched.as_lisp_string()?;
    let byte_start = char_pos_to_byte_lisp_string(string, start);
    let byte_end = char_pos_to_byte_lisp_string(string, end);
    string
        .slice(byte_start, byte_end)
        .and_then(|slice| slice.as_utf8_str().map(str::to_owned))
}

// -----------------------------------------------------------------------
// translate_emacs_regex
// -----------------------------------------------------------------------

#[test]
fn translate_groups() {
    crate::test_utils::init_test_tracing();
    // Emacs \( \) → Rust ( )
    assert_eq!(translate_emacs_regex("\\(foo\\)"), "(foo)");
}

#[test]
fn translate_alternation() {
    crate::test_utils::init_test_tracing();
    // Emacs \| → Rust |
    assert_eq!(translate_emacs_regex("foo\\|bar"), "foo|bar");
}

#[test]
fn translate_literal_parens() {
    crate::test_utils::init_test_tracing();
    // Emacs literal ( ) → Rust \( \)
    assert_eq!(translate_emacs_regex("(foo)"), "\\(foo\\)");
}

#[test]
fn translate_literal_braces() {
    crate::test_utils::init_test_tracing();
    // Emacs literal { } → Rust \{ \}
    assert_eq!(translate_emacs_regex("{3}"), "\\{3\\}");
}

#[test]
fn translate_repetition_braces() {
    crate::test_utils::init_test_tracing();
    // Emacs \{3\} → Rust {3}
    assert_eq!(translate_emacs_regex("a\\{3\\}"), "a{3}");
}

#[test]
fn translate_literal_pipe() {
    crate::test_utils::init_test_tracing();
    // Emacs literal | → Rust \|
    assert_eq!(translate_emacs_regex("a|b"), "a\\|b");
}

#[test]
fn translate_word_boundary() {
    crate::test_utils::init_test_tracing();
    // Emacs \< \> → Rust \b
    assert_eq!(translate_emacs_regex("\\<word\\>"), "\\bword\\b");
}

#[test]
fn translate_symbol_boundary() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("\\_<word\\_>"), "\\bword\\b");
}

#[test]
fn translate_buffer_boundaries() {
    crate::test_utils::init_test_tracing();
    // Emacs \` → Rust \A, Emacs \' → Rust \z
    assert_eq!(translate_emacs_regex("\\`foo\\'"), "\\Afoo\\z");
}

#[test]
fn translate_character_class_passthrough() {
    crate::test_utils::init_test_tracing();
    // Character classes should pass through mostly unchanged
    assert_eq!(translate_emacs_regex("[a-z]"), "[a-z]");
    assert_eq!(translate_emacs_regex("[^0-9]"), "[^0-9]");
}

#[test]
fn translate_character_class_backslash_ranges_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("[+\\-*/=<>]"), "[+/=<>]");
}

#[test]
fn translate_easymenu_command_hint_regexp() {
    crate::test_utils::init_test_tracing();
    let emacs = r"^[^\]*\(\\\[\([^]]+\)]\)[^\]*$";
    assert_eq!(
        translate_emacs_regex(emacs),
        r"^[^\\]*(\\\[([^\]]+)])[^\\]*$"
    );
}

#[test]
fn replace_match_case_capitalizes_each_word_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(apply_match_case("[alice:5]", "Alice"), "[Alice:5]");
    assert_eq!(
        apply_match_case("h_hello w_world", "Hello World"),
        "H_Hello W_World"
    );
}

#[test]
fn replace_match_case_upcases_all_caps_matches() {
    crate::test_utils::init_test_tracing();
    assert_eq!(apply_match_case("foo-bar", "FOO"), "FOO-BAR");
}

#[test]
fn translate_reversed_range_classes() {
    crate::test_utils::init_test_tracing();
    // Reversed ranges are empty in Emacs.
    assert_eq!(translate_emacs_regex("[z-a]"), "[^\\s\\S]");
    assert_eq!(translate_emacs_regex("[^z-a]"), "[\\s\\S]");
}

#[test]
fn translate_backslash_w() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("\\w+"), "\\w+");
}

#[test]
fn compile_search_pattern_uses_backref_engine_for_supported_captures() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\([a-z]+\\)-\\([0-9]+\\)", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_uses_backref_engine_for_noncapturing_groups() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\(?:foo\\|bar\\)+", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_syntax_classes_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\(defun\\|defvar\\)\\s-+\\(\\w+\\)", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_category_classes_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("[ \t]\\|\\c|.\\|.\\c|", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_digit_classes_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\d+", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_char_class_escapes_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("[\\w-]+", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
    assert!(matches!(
        compile_search_pattern("[\\s-]+", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_lazy_quantifiers_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("a.*?b", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
    assert!(matches!(
        compile_search_pattern("a\\{2,4\\}?b", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_open_interval_quantifiers_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("a\\{,2\\}b", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_explicit_numbered_groups_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\(?1:[^}]*\\)", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
    assert!(matches!(
        compile_search_pattern("\\(?9:.*?\\)", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_symbol_boundaries_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\_<foo\\_>", false),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn compile_search_pattern_routes_bracket_section_anchor_through_backref_engine() {
    crate::test_utils::init_test_tracing();
    assert!(matches!(
        compile_search_pattern("\\`\\[\\([^]]+\\)\\]\\'", true),
        Ok(CompiledSearchPattern::Emacs(_))
    ));
}

#[test]
fn string_match_supported_capture_pattern_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\sw+", "foo_bar", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 7)));
}

#[test]
fn string_match_category_escape_pattern_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\c|.", "éx", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 2)));
}

#[test]
fn string_match_match_at_point_escape_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\=foo", "foo", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn string_match_match_at_point_escape_respects_nonzero_start() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\=foo", "xxfoo", 2, false, &mut md);
    assert_eq!(result, Ok(Some(2)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((2, 5)));
}

#[test]
fn string_match_match_at_point_escape_does_not_skip_past_start() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\=foo", "xxafoo", 2, false, &mut md);
    assert_eq!(result, Ok(None));
    assert!(md.is_none());
}

#[test]
fn string_match_digit_escape_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\d+", "123x", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn string_match_control_escape_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("a\\tb", "a\tb", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

// Regex audit #6: `\cX` category-spec covers the common Unicode
// blocks (Han, Hiragana, Katakana, Hangul, Latin, ...) instead of
// returning false for everything except `\c|`. GNU populates the
// category table from `lisp/international/characters.el`; we
// hardcode the same Unicode block ranges in
// `default_char_has_category`.
//
// Verified against GNU Emacs 31.0.50:
//
//   (string-match "\\cC" "中") => 0     (Han ideograph)
//   (string-match "\\cC" "a")  => nil
//   (string-match "\\cH" "あ") => 0     (Hiragana)
//   (string-match "\\cK" "ア") => 0     (Katakana)
//   (string-match "\\ch" "한") => 0     (Korean Hangul)
//   (string-match "\\cl" "a")  => 0     (Latin)

#[test]
fn category_han_matches_cjk_unified_ideographs() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    assert_eq!(string_match_full("\\cC", "中", 0, &mut md), Ok(Some(0)));
    let mut md = None;
    assert_eq!(string_match_full("\\cC", "a", 0, &mut md), Ok(None));
}

#[test]
fn category_hiragana_matches_japanese_hiragana() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    assert_eq!(string_match_full("\\cH", "あ", 0, &mut md), Ok(Some(0)));
    let mut md = None;
    assert_eq!(string_match_full("\\cH", "ア", 0, &mut md), Ok(None));
}

#[test]
fn category_katakana_matches_japanese_katakana() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    assert_eq!(string_match_full("\\cK", "ア", 0, &mut md), Ok(Some(0)));
    let mut md = None;
    assert_eq!(string_match_full("\\cK", "あ", 0, &mut md), Ok(None));
}

#[test]
fn category_hangul_matches_korean_hangul() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    assert_eq!(string_match_full("\\ch", "한", 0, &mut md), Ok(Some(0)));
    let mut md = None;
    assert_eq!(string_match_full("\\ch", "中", 0, &mut md), Ok(None));
}

#[test]
fn category_latin_matches_ascii_letters() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    assert_eq!(string_match_full("\\cl", "a", 0, &mut md), Ok(Some(0)));
    let mut md = None;
    assert_eq!(string_match_full("\\cl", "中", 0, &mut md), Ok(None));
}

// Regex audit #2: POSIX longest-match. GNU's `posix-*` family passes
// `posix = 1` through `compile_pattern` into `re_match_2_internal`;
// the matcher then tracks the best (longest) match across all
// backtracks (regex-emacs.c:4143-4344) and returns it via the
// "restore best" label at line 4325 when backtracking exhausts.
// Before this fix neomacs ignored the flag and returned the
// leftmost-first match for `posix-*` calls. Reference shape from
// GNU Emacs 31.0.50:
//
//   (string-match "a\\|aa\\|aaa" "aaaa")       => 0, m0="a"
//   (posix-string-match "a\\|aa\\|aaa" "aaaa") => 0, m0="aaa"
//   (string-match "\\(a\\|ab\\|abc\\)" "abcdef")       => 0, m0="a"
//   (posix-string-match "\\(a\\|ab\\|abc\\)" "abcdef") => 0, m0="abc"

#[test]
fn string_match_alternation_takes_leftmost_first_without_posix() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("a\\|aa\\|aaa", "aaaa", 0, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(
        md.groups[0],
        Some((0, 1)),
        "non-POSIX picks first alternative"
    );
}

#[test]
fn string_match_alternation_prefers_longest_under_posix_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result =
        string_match_full_with_case_fold_and_posix("a\\|aa\\|aaa", "aaaa", 0, false, true, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(
        md.groups[0],
        Some((0, 3)),
        "POSIX picks the longest alternative"
    );
}

#[test]
fn string_match_grouped_alternation_leftmost_first_without_posix() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("\\(a\\|ab\\|abc\\)", "abcdef", 0, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 1)));
    assert_eq!(md.groups[1], Some((0, 1)));
}

#[test]
fn string_match_grouped_alternation_longest_under_posix_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold_and_posix(
        "\\(a\\|ab\\|abc\\)",
        "abcdef",
        0,
        false,
        true,
        &mut md,
    );
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
    assert_eq!(md.groups[1], Some((0, 3)));
}

#[test]
fn posix_longest_match_returns_match_when_non_posix_path_would_also_match() {
    // Sanity: even when the non-POSIX leftmost-first result is
    // already the longest, the POSIX path must still return it
    // (rather than returning None because the "backtrack harder"
    // logic couldn't beat it).
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold_and_posix("foo", "foo", 0, false, true, &mut md);
    assert_eq!(result, Ok(Some(0)));
    assert_eq!(md.unwrap().groups[0], Some((0, 3)));
}

// Regex audit #10: backslash is LITERAL inside a bracket expression
// in GNU `regex-emacs.c` (see the charset parser at lines 2055-2140,
// which has no escape handling). Before the fix neomacs expanded
// `\w`, `\W`, `\s-`, `\d`, `\D` inside `[...]` to their out-of-
// bracket meanings, and these tests asserted that divergent
// behavior. They now assert the GNU meaning. For the union-with-dash
// that the old tests were really trying to express, use the POSIX
// class form as shown in the `posix_class_*` tests added for
// audit #7. Verified with GNU Emacs 31.0.50:
//
//   (string-match "[\\w-]+" "foo-bar!") => 3
//   (string-match "[\\s-]+" " \tfoo")   => nil
//   (string-match "[[:word:]-]+" "foo-bar!") => 0
#[test]
fn string_match_backslash_w_in_charset_is_literal_like_gnu() {
    crate::test_utils::init_test_tracing();
    // `[\w-]+` is the set {`\`, `w`, `-`}. Against "foo-bar!" the
    // first character in that set is the `-` at position 3.
    let mut md = None;
    let result = string_match_full_with_case_fold("[\\w-]+", "foo-bar!", 0, false, &mut md);
    assert_eq!(result, Ok(Some(3)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((3, 4)));
}

#[test]
fn string_match_backslash_w_in_charset_matches_literal_backslash_and_w() {
    crate::test_utils::init_test_tracing();
    // Sanity: `[\w]` matches a literal `\` or `w`.
    let mut md = None;
    let result = string_match_full_with_case_fold("[\\w]", "w", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));

    let mut md = None;
    let result = string_match_full_with_case_fold("[\\w]", "\\", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));

    // A char that is neither `\` nor `w` must not match.
    let mut md = None;
    let result = string_match_full_with_case_fold("[\\w]", "a", 0, false, &mut md);
    assert_eq!(result, Ok(None));
}

#[test]
fn string_match_backslash_s_in_charset_is_literal_like_gnu() {
    crate::test_utils::init_test_tracing();
    // `[\s-]+` is the set {`\`, `s`, `-`}. " \tfoo" contains none of
    // those at any position, so GNU returns nil.
    let mut md = None;
    let result = string_match_full_with_case_fold("[\\s-]+", " \tfoo", 0, false, &mut md);
    assert_eq!(result, Ok(None));
}

// The POSIX-class form is the GNU-sanctioned replacement for the
// old `[\w-]+` / `[\s-]+` workaround patterns. These tests document
// the supported way to express the same intent.
#[test]
fn string_match_posix_word_class_with_dash_range_matches_identifiers() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("[[:word:]-]+", "foo-bar!", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 7)));
}

#[test]
fn string_match_posix_space_class_with_dash_range_matches_whitespace_runs() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("[[:space:]-]+", " \tfoo", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 2)));
}

#[test]
fn string_match_lazy_quantifier_preserves_fallback_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("a.*?b", "aXXbYYb", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 4)));
}

#[test]
fn string_match_lazy_plus_quantifier_prefers_shorter_match() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("a.+?b", "aXXbYYb", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 4)));
}

#[test]
fn string_match_lazy_optional_quantifier_prefers_zero_width_choice() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("ab??c", "abc", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn string_match_lazy_counted_quantifier_prefers_shorter_match() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("a\\{2,4\\}?b", "aaaab", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 5)));
}

#[test]
fn string_match_open_interval_quantifier_matches_gnu_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("a\\{,2\\}b", "aab", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn string_match_explicit_numbered_group_preserves_group_slot() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\(?9:[A-Z]+\\)", "xxABCyy", 0, false, &mut md);
    assert_eq!(result, Ok(Some(2)));
    let md = md.expect("match data");
    assert_eq!(md.groups.len(), 10);
    assert_eq!(md.groups[0], Some((2, 5)));
    assert!(md.groups[1..9].iter().all(Option::is_none));
    assert_eq!(md.groups[9], Some((2, 5)));
}

#[test]
fn string_match_symbol_boundary_pattern_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("\\_<foo\\_>", "x foo y", 0, false, &mut md);
    assert_eq!(result, Ok(Some(2)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((2, 5)));
}

#[test]
fn string_match_posix_upper_class_folds_to_alpha_under_case_fold() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result =
        string_match_full_with_case_fold("[[:upper:]]+", "helloWORLDfoo", 0, true, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 13)));
}

#[test]
fn string_match_posix_upper_class_folds_to_alpha_on_lisp_string() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let string = LispString::new("helloWORLDfoo".to_string(), false);
    let result = string_match_full_with_case_fold_source_lisp(
        "[[:upper:]]+",
        &string,
        SearchedString::Owned(LispString::from_utf8("helloWORLDfoo")),
        0,
        true,
        &mut md,
    );
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 13)));
}

// Regex audit #7: the 4 previously missing POSIX classes
// (word, nonascii, unibyte, multibyte) and the space/blank and
// print/graph splits must match GNU `regex-emacs.c:1525-1630`
// (`re_wctype_parse` + `re_iswctype`) exactly.

// Regex audit #8: `[[:word:]]` (and `[[:space:]]`) consult the
// buffer's syntax table at MATCH time, so per-mode overrides like
// "`_` is Sword in python-mode" extend the charset. The matcher
// takes the union of the bitmap and the class bits driven through
// the buffer syntax table.
//
// Verified against GNU Emacs 31.0.50:
//
//   (with-temp-buffer
//     (modify-syntax-entry ?_ "w")
//     (insert "foo_bar")
//     (goto-char 1)
//     (looking-at "[[:word:]]+")
//     (match-end 0))    ; => 8 (whole "foo_bar")
#[test]
fn posix_word_class_extends_via_buffer_syntax_table_override() {
    crate::test_utils::init_test_tracing();
    use crate::emacs_core::syntax::{SyntaxClass, SyntaxEntry};

    let mut buf = make_test_buffer("foo_bar baz");
    // GNU-parity isolation: give this buffer its own copy of the
    // standard chartable so the mutation doesn't leak into other
    // buffers / tests.
    crate::emacs_core::syntax::SyntaxTable::isolate_for_buffer(&mut buf)
        .modify_syntax_entry('_', SyntaxEntry::simple(SyntaxClass::Word));
    buf.goto_byte(0);

    let mut md = None;
    let matched = looking_at(&buf, "[[:word:]]+", false, &mut md).expect("compile ok");
    assert!(matched, "[[:word:]]+ should match `foo_bar`");
    let md = md.unwrap();
    assert_eq!(
        md.groups[0],
        Some((0, 7)),
        "match should cover the whole `foo_bar`"
    );

    // Without the override, `_` is Symbol (not Word) in the
    // standard syntax table, so the match stops at index 3.
    let mut buf2 = make_test_buffer("foo_bar baz");
    buf2.goto_byte(0);
    let mut md = None;
    let matched = looking_at(&buf2, "[[:word:]]+", false, &mut md).expect("compile ok");
    assert!(matched);
    assert_eq!(
        md.unwrap().groups[0],
        Some((0, 3)),
        "without override, match stops at `_`"
    );
}

#[test]
fn posix_class_word_matches_ascii_letters_and_digits_but_not_punct() {
    crate::test_utils::init_test_tracing();
    // Default standard-syntax word constituents: a-z A-Z 0-9. `_`,
    // `-`, and ASCII space are NOT word constituents in the standard
    // table so `[[:word:]]` must not match them. (Audit #8 tracks
    // threading the per-buffer syntax table through the matcher; in
    // default/standard syntax this is the GNU baseline.)
    let mut md = None;
    let r = string_match_full("[[:word:]]+", "foo42bar", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));
    assert_eq!(md.unwrap().groups[0], Some((0, 8)));

    let mut md = None;
    let r = string_match_full("[[:word:]]+", "!!!abc!!!", 0, &mut md);
    assert_eq!(r, Ok(Some(3)));
    assert_eq!(md.unwrap().groups[0], Some((3, 6)));

    // `_` is symbol, not word, in the standard table -> does not match.
    let mut md = None;
    let r = string_match_full("^[[:word:]]+$", "_", 0, &mut md);
    assert_eq!(r, Ok(None));
}

#[test]
fn posix_class_nonascii_matches_only_chars_at_or_above_u0080() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let r = string_match_full("[[:nonascii:]]+", "abcéfg", 0, &mut md);
    assert_eq!(r, Ok(Some(3)));
    // `é` occupies one character slot (md positions are char indices
    // for string search).
    assert_eq!(md.unwrap().groups[0], Some((3, 4)));

    // Pure ASCII input -> no match.
    let mut md = None;
    let r = string_match_full("[[:nonascii:]]", "abc123", 0, &mut md);
    assert_eq!(r, Ok(None));
}

#[test]
fn posix_class_multibyte_matches_only_non_ascii_chars() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let r = string_match_full("[[:multibyte:]]+", "abcé", 0, &mut md);
    assert_eq!(r, Ok(Some(3)));
    assert_eq!(md.unwrap().groups[0], Some((3, 4)));

    let mut md = None;
    let r = string_match_full("[[:multibyte:]]", "x", 0, &mut md);
    assert_eq!(r, Ok(None));
}

#[test]
fn posix_class_unibyte_matches_every_ascii_char() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let r = string_match_full("[[:unibyte:]]+", "abc", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));
    assert_eq!(md.unwrap().groups[0], Some((0, 3)));
}

#[test]
fn posix_class_blank_is_only_space_and_tab_unlike_space() {
    crate::test_utils::init_test_tracing();
    // GNU ISBLANK: space and tab only. A newline must NOT match
    // `[[:blank:]]` but MUST match `[[:space:]]`. Before the audit
    // #7 fix, neomacs merged the two classes so this distinction was
    // silently wrong.
    let mut md = None;
    let r = string_match_full("[[:blank:]]", "\n", 0, &mut md);
    assert_eq!(r, Ok(None));

    let mut md = None;
    let r = string_match_full("[[:space:]]", "\n", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));

    let mut md = None;
    let r = string_match_full("[[:blank:]]", " ", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));

    let mut md = None;
    let r = string_match_full("[[:blank:]]", "\t", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));
}

#[test]
fn posix_class_print_includes_space_but_graph_excludes_it() {
    crate::test_utils::init_test_tracing();
    // GNU ISPRINT: c >= ' '. GNU ISGRAPH: c > ' '. The two classes
    // must differ on the space character. Before the fix neomacs
    // merged them so `[[:graph:]]` matched space.
    let mut md = None;
    let r = string_match_full("[[:print:]]", " ", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));

    let mut md = None;
    let r = string_match_full("[[:graph:]]", " ", 0, &mut md);
    assert_eq!(r, Ok(None));

    // Both classes must still match `a`.
    let mut md = None;
    let r = string_match_full("[[:graph:]]", "a", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));
    let mut md = None;
    let r = string_match_full("[[:print:]]", "a", 0, &mut md);
    assert_eq!(r, Ok(Some(0)));
}

#[test]
fn posix_class_unknown_name_signals_compile_error_like_gnu() {
    crate::test_utils::init_test_tracing();
    // GNU re_wctype_parse returns RECC_ERROR for unknown names and
    // the caller signals REG_ECTYPE (regex-emacs.c:1600, 2071). We
    // raise the equivalent Rust-level compile error instead of
    // silently ignoring the unknown class name.
    let mut md = None;
    let r = string_match_full("[[:notaclass:]]", "abc", 0, &mut md);
    assert!(r.is_err(), "expected compile error, got {:?}", r);
}

#[test]
fn string_match_anchored_operator_char_class_mirrors_gnu_bracket_closing() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result =
        string_match_full_with_case_fold("\\`[-+*/=<>!&|(){}\\[\\];,.]", "=", 0, true, &mut md);
    assert_eq!(result, Ok(None));
    assert!(md.is_none());
}

#[test]
fn string_match_anchored_operator_char_class_on_lisp_slice_mirrors_gnu_bracket_closing() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let source = LispString::new("x = 42;".to_string(), false);
    let slice = source.slice(2, source.byte_len()).expect("slice");
    let result = string_match_full_with_case_fold_source_lisp(
        "\\`[-+*/=<>!&|(){}\\[\\];,.]",
        &slice,
        SearchedString::Owned(slice.clone()),
        0,
        true,
        &mut md,
    );
    assert_eq!(result, Ok(None));
    assert!(md.is_none());
}

#[test]
fn owned_raw_unibyte_match_data_preserves_bytes() {
    crate::test_utils::init_test_tracing();
    let pattern = LispString::from_unibyte(vec![0xFF]);
    let haystack = LispString::from_unibyte(vec![0x80, 0xFF, 0x81]);
    let mut md = None;
    let result = string_match_full_with_case_fold_source_lisp_pattern_posix(
        &pattern,
        &haystack,
        SearchedString::Owned(haystack.clone()),
        0,
        true,
        false,
        &mut md,
    );
    assert_eq!(result, Ok(Some(1)));
    let md = md.expect("match data");
    let searched = md.searched_string.expect("searched string");
    let string = searched.as_lisp_string().expect("lisp string");
    let (start, end) = md.groups[0].expect("full match");
    let byte_start = char_pos_to_byte_lisp_string(string, start);
    let byte_end = char_pos_to_byte_lisp_string(string, end);
    let slice = string.slice(byte_start, byte_end).expect("slice");
    assert!(!slice.is_multibyte());
    assert_eq!(slice.as_bytes(), &[0xFF]);
}

#[test]
fn heap_match_string_on_lisp_slice_mirrors_gnu_bracket_closing() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let source = LispString::new("x = 42;".to_string(), false);
    let slice = source.slice(2, source.byte_len()).expect("slice");
    let slice_val = crate::emacs_core::value::Value::string(slice.as_utf8_str().unwrap_or(""));
    let stored_slice = slice_val.as_lisp_string().unwrap().clone();
    let result = string_match_full_with_case_fold_source_lisp(
        "\\`[-+*/=<>!&|(){}\\[\\];,.]",
        &stored_slice,
        SearchedString::Heap(slice_val),
        0,
        true,
        &mut md,
    );
    assert_eq!(result, Ok(None));
    assert!(md.is_none());
}

#[test]
fn heap_tokenizer_loop_mirrors_gnu_single_char_operator_behavior() {
    crate::test_utils::init_test_tracing();
    let code = LispString::new(
        "let x = 42; if x >= 10 && x != 0 { return x + 1; }".to_string(),
        false,
    );
    let keywords = ["if", "else", "while", "return", "let", "fn"];
    let patterns = [
        ("\\`[ \t\n]+", "skip"),
        ("\\`[0-9]+\\(?:\\.[0-9]+\\)?", "number"),
        ("\\`\"[^\"]*\"", "string"),
        ("\\`\\(?:==\\|!=\\|<=\\|>=\\|&&\\|||\\|->\\)", "operator"),
        ("\\`[-+*/=<>!&|(){}\\[\\];,.]", "operator"),
        ("\\`[a-zA-Z_][a-zA-Z0-9_]*", "identifier"),
    ];

    let mut pos = 0usize;
    let mut tokens = Vec::new();
    while pos < code.byte_len() {
        let rest = code.slice(pos, code.byte_len()).expect("rest slice");
        let rest_val = crate::emacs_core::value::Value::string(rest.as_utf8_str().unwrap_or(""));
        let stored_rest = rest_val.as_lisp_string().unwrap().clone();
        let mut matched = false;

        for (pattern, mut kind) in patterns {
            if matched {
                break;
            }

            let mut md = None;
            if let Ok(Some(_)) = string_match_full_with_case_fold_source_lisp(
                pattern,
                &stored_rest,
                SearchedString::Heap(rest_val),
                0,
                true,
                &mut md,
            ) {
                let md = md.expect("match data");
                let text = extract_heap_match_string(&md, 0).expect("matched text");
                pos += text.len();
                if kind != "skip" {
                    if kind == "identifier" && keywords.contains(&text.as_str()) {
                        kind = "keyword";
                    }
                    tokens.push((kind.to_string(), text));
                }
                matched = true;
            }
        }

        if !matched {
            pos += 1;
        }
    }

    assert_eq!(
        tokens,
        vec![
            ("keyword".to_string(), "let".to_string()),
            ("identifier".to_string(), "x".to_string()),
            ("number".to_string(), "42".to_string()),
            ("keyword".to_string(), "if".to_string()),
            ("identifier".to_string(), "x".to_string()),
            ("operator".to_string(), ">=".to_string()),
            ("number".to_string(), "10".to_string()),
            ("operator".to_string(), "&&".to_string()),
            ("identifier".to_string(), "x".to_string()),
            ("operator".to_string(), "!=".to_string()),
            ("number".to_string(), "0".to_string()),
            ("keyword".to_string(), "return".to_string()),
            ("identifier".to_string(), "x".to_string()),
            ("number".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn string_match_bracket_section_anchor_pattern_matches_whole_string() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result =
        string_match_full_with_case_fold("\\`\\[\\([^]]+\\)\\]\\'", "[database]", 0, true, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 10)));
    assert_eq!(md.groups[1], Some((1, 9)));
}

#[test]
fn string_match_line_anchor_pattern_uses_backref_engine_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("^foo$", "foo", 0, false, &mut md);
    assert_eq!(result, Ok(Some(0)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((0, 3)));
}

#[test]
fn string_match_line_anchor_pattern_respects_multiline_semantics() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full_with_case_fold("^foo$", "a\nfoo\nb", 0, false, &mut md);
    assert_eq!(result, Ok(Some(2)));
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((2, 5)));
}

#[test]
fn translate_complex_pattern() {
    crate::test_utils::init_test_tracing();
    // Emacs: \(defun\|defvar\)\s-+\(\w+\)
    // Rust:  (defun|defvar)\s+(\w+)
    let emacs = "\\(defun\\|defvar\\)\\s-+\\(\\w+\\)";
    let rust = translate_emacs_regex(emacs);
    // After translation: (defun|defvar)\s+(\w+)
    assert_eq!(rust, "(defun|defvar)\\s+(\\w+)");
}

#[test]
fn translate_explicit_numbered_group_keeps_fallback_compilable() {
    crate::test_utils::init_test_tracing();
    let emacs = "\\(?9:.*?\\)";
    assert_eq!(translate_emacs_regex(emacs), "(.*?)");
}

#[test]
fn translate_open_interval_quantifier_keeps_fallback_compilable() {
    crate::test_utils::init_test_tracing();
    let emacs = "a\\{,2\\}b";
    assert_eq!(translate_emacs_regex(emacs), "a{0,2}b");
}

#[test]
fn translate_category_escape_keeps_fill_patterns_compilable() {
    crate::test_utils::init_test_tracing();
    let emacs = "[ \t]\\|\\c|.\\|.\\c|";
    let rust = translate_emacs_regex(emacs);
    assert_eq!(rust, "[ \t]|[^\\x00-\\x7F].|.[^\\x00-\\x7F]");
}

#[test]
fn translate_empty_pattern() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex(""), "");
}

#[test]
fn translate_no_special_chars() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("hello"), "hello");
}

#[test]
fn translate_escaped_backslash() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("\\\\"), "\\\\");
}

#[test]
fn translate_multibyte_literals() {
    crate::test_utils::init_test_tracing();
    assert_eq!(translate_emacs_regex("\\(é\\)"), "(é)");
    assert_eq!(translate_emacs_regex("[éx]"), "[éx]");
    assert_eq!(translate_emacs_regex("\\é"), "é");
    assert_eq!(translate_emacs_regex("\\😀"), "😀");
}

#[test]
fn trivial_regexp_matches_gnu_meta_rules() {
    crate::test_utils::init_test_tracing();
    assert!(trivial_regexp_p("hello\\.txt"));
    assert!(trivial_regexp_p("\\😀"));
    assert!(!trivial_regexp_p("he.*o"));
    assert!(!trivial_regexp_p("\\(group\\)"));
    assert!(!trivial_regexp_p("\\1"));
    assert!(!trivial_regexp_p("trailing\\"));
}

// -----------------------------------------------------------------------
// string_match_full
// -----------------------------------------------------------------------

#[test]
fn string_match_basic() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("\\é", "aéx", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
}

#[test]
fn string_match_trivial_escaped_literal_uses_character_positions() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("\\.", "a.b", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((1, 2)));
}

#[test]
fn string_match_backreference_reuses_captured_text() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let matched = looking_at_string("\\(x\\)\\1\\1", "xxx!", false, &mut md).unwrap();
    assert!(matched);
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((0, 3)));
    assert_eq!(md.groups[1], Some((0, 1)));
}

#[test]
fn re_search_forward_backreference_word_boundary() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full(r"{{\([^}]+\)}}", "x {{name}} y", 0, &mut md).unwrap();
    assert_eq!(result, Some(2));
    let md = md.unwrap();
    assert_eq!(md.groups[0], Some((2, 10)));
    assert_eq!(md.groups[1], Some((4, 8)));
}

#[test]
fn string_match_template_foreach_pattern() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("world", "hello world", 6, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(6));
}

#[test]
fn string_match_no_match() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let result = string_match_full("xyz", "hello world", 0, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
    assert!(md.is_none());
}

#[test]
fn string_match_emacs_alternation() {
    crate::test_utils::init_test_tracing();
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
    let mut buf = Buffer::new(BufferId(1), Value::string("test"));
    buf.insert(text);
    // Reset point to beginning
    buf.goto_byte(0);
    // zv was updated by insert
    buf
}

#[test]
fn search_forward_basic() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let result = search_forward(&mut buf, "xyz", None, true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
    assert_eq!(buf.pt, 0); // point unchanged
}

#[test]
fn search_forward_not_found_error() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    let result = search_forward(&mut buf, "xyz", None, false, false, &mut md);
    assert!(result.is_err());
}

#[test]
fn search_forward_case_fold_true() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("A");
    let mut md = None;
    let result = search_forward(&mut buf, "a", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(1));
}

#[test]
fn search_forward_case_fold_true_unicode_literal() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("Äx");
    let mut md = None;
    let result = search_forward(&mut buf, "ä", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some('Ä'.len_utf8()));
}

#[test]
fn re_search_forward_trivial_regexp_follows_literal_case_fold_path() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    let mut md = None;
    // Search only within first 5 bytes — "world" starts at 6 so should not be found
    let result = search_forward(&mut buf, "world", Some(5), true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[test]
fn search_forward_from_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("aaa bbb aaa");
    buf.goto_byte(4); // after "aaa "
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
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(11); // end of buffer
    let mut md = None;
    let result = search_backward(&mut buf, "hello", None, false, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(0)); // beginning of "hello"
    assert_eq!(buf.pt, 0);
}

#[test]
fn search_backward_not_found() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(11);
    let mut md = None;
    let result = search_backward(&mut buf, "xyz", None, true, false, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), None);
}

#[test]
fn search_backward_finds_last_occurrence() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("aaa bbb aaa");
    buf.goto_byte(11); // end
    let mut md = None;
    let result = search_backward(&mut buf, "aaa", None, false, false, &mut md);
    assert!(result.is_ok());
    // Should find the LAST "aaa" (at position 8)
    assert_eq!(result.unwrap(), Some(8));
    assert_eq!(buf.pt, 8);
}

#[test]
fn search_backward_case_fold_true_unicode_literal() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("Ää");
    buf.goto_byte("Ää".len());
    let mut md = None;
    let result = search_backward(&mut buf, "ä", None, false, true, &mut md);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some('Ä'.len_utf8()));
    assert_eq!(buf.pt_byte, 'Ä'.len_utf8());
    assert_eq!(buf.pt, 1);
}

// -----------------------------------------------------------------------
// Buffer search: re_search_forward
// -----------------------------------------------------------------------

#[test]
fn re_search_forward_basic() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("abc 123 def 456");
    buf.goto_byte(15); // end
    let mut md = None;
    let result = re_search_backward(&mut buf, "[0-9]+", None, false, false, &mut md);
    assert!(result.is_ok());
    // GNU re-search-backward scans positions backward and matches at the
    // first position where the regex succeeds.  From point-max (15/0-indexed=14),
    // position 14 is '6' which matches [0-9]+.  So match-beginning is 14.
    assert_eq!(result.unwrap(), Some(14));
    assert_eq!(buf.pt, 14);
}

#[test]
fn re_search_backward_finds_nullable_match_at_point() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("abc\n");
    buf.goto_byte(3); // point before trailing newline
    let mut md = None;
    let result = re_search_backward(&mut buf, "\\(?:$\\)\\=", Some(0), true, false, &mut md);
    assert_eq!(result, Ok(Some(3)));
    assert_eq!(buf.pt, 3);
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((3, 3)));
}

#[test]
fn re_search_forward_finds_nullable_match_at_buffer_end() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("abc");
    buf.goto_byte(3);
    let mut md = None;
    let result = re_search_forward(&mut buf, "\\=", None, true, false, &mut md);
    assert_eq!(result, Ok(Some(3)));
    assert_eq!(buf.pt, 3);
    let md = md.expect("match data");
    assert_eq!(md.groups[0], Some((3, 3)));
}

// -----------------------------------------------------------------------
// looking_at
// -----------------------------------------------------------------------

#[test]
fn looking_at_matches() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(0);
    let mut md = None;
    let result = looking_at(&buf, "hello", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
    assert!(md.is_some());
}

#[test]
fn looking_at_no_match() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(0);
    let mut md = None;
    let result = looking_at(&buf, "world", true, &mut md);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn looking_at_from_middle() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(6); // "world"
    let mut md = None;
    let result = looking_at(&buf, "world", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn looking_at_defaults_to_case_fold() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("A");
    buf.goto_byte(0);
    let mut md = None;
    let result = looking_at(&buf, "a", true, &mut md);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn looking_at_respects_case_fold_false() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("A");
    buf.goto_byte(0);
    let mut md = None;
    let result = looking_at(&buf, "a", false, &mut md);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn looking_at_with_groups() {
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("foo123bar");
    buf.goto_byte(0);
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut buf = make_test_buffer("hello world");
    buf.goto_byte(0);
    let mut md = None;
    // Match "hello" with a group
    let _ = re_search_forward(&mut buf, "\\(hello\\)", None, false, false, &mut md);
    let result = replace_match_buffer(&mut buf, "\\1 there", false, false, 0, &md);
    assert!(result.is_ok());
    let content = buf.text.text_range(0, buf.text.len());
    assert_eq!(content, "hello there world");
}

#[test]
fn replace_match_buffer_preserves_unibyte_raw_bytes() {
    crate::test_utils::init_test_tracing();
    let mut buf = Buffer::new(BufferId(1), Value::string("raw"));
    buf.set_multibyte_value(false);
    buf.insert_lisp_string(&crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    buf.goto_byte(0);

    let mut md = None;
    let result = re_search_forward(&mut buf, ".", None, false, false, &mut md);
    assert_eq!(result, Ok(Some(1)));

    let result = replace_match_buffer(&mut buf, "\\&", false, false, 0, &md);
    assert!(result.is_ok());

    let content = buf.buffer_substring_lisp_string(0, buf.total_bytes());
    assert!(!content.is_multibyte());
    assert_eq!(content.as_bytes(), &[0xFF]);
    assert_eq!(buf.total_bytes(), 1);
}

#[test]
fn replace_match_applies_case_pattern() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("\\([a-z]+\\)\\([0-9]+\\)", "abc123", 0, &mut md);
    let replaced = replace_match_string("abc123", "X", false, false, 2, &md).unwrap();
    assert_eq!(replaced, "abcX");
}

#[test]
fn replace_match_subexp_errors_when_missing() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("\\([a-z]+\\)?\\([0-9]+\\)", "123", 0, &mut md);
    let err = replace_match_string("123", "X", false, false, 1, &md).unwrap_err();
    assert_eq!(err, REPLACE_MATCH_SUBEXP_MISSING);
}

#[test]
fn replace_match_preserves_multibyte_replacement_literals() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("x", "x", 0, &mut md);
    let replaced = replace_match_string("x", "éz", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "éz");
}

#[test]
fn replace_match_preserves_multibyte_replacement_with_backref() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("\\(x\\)", "x", 0, &mut md);
    let replaced = replace_match_string("x", "\\1é", false, false, 0, &md).unwrap();
    assert_eq!(replaced, "xé");
}

// Regex audit #11: GNU `Freplace_match` rejects `\0` in the non-literal
// replacement template. search.c:2565 and search.c:2703 both require
// `c >= '1' && c <= '9'`; `\0` falls through to the
// `"Invalid use of `\\' in replacement text"` error at search.c:2584
// and search.c:2713. Before the fix neomacs's `build_replacement`
// matched `'0'..='9'` and returned the whole match for `\0`.
#[test]
fn replace_match_rejects_backslash_zero_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("foo", "foo", 0, &mut md);
    let err = replace_match_string("foo", "\\0", false, false, 0, &md)
        .expect_err("\\0 must be rejected by replace-match");
    assert_eq!(err, "Invalid use of `\\' in replacement text");
}

// Regex audit #12: GNU signals an error on unknown backslash escapes
// in the replacement template (search.c:2584 and search.c:2713).
// Before the fix neomacs's catch-all silently emitted the literal
// `\X`. `\?` is the sole exception (search.c:2583) and is passed
// through literally — see `replace_match_passes_backslash_question_literally`.
#[test]
fn replace_match_rejects_unknown_backslash_escape_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("foo", "foo", 0, &mut md);

    // `\n` must error, not emit literal `\n`.
    let err = replace_match_string("foo", "a\\nb", false, false, 0, &md)
        .expect_err("\\n in replacement must be rejected");
    assert_eq!(err, "Invalid use of `\\' in replacement text");

    // An arbitrary ASCII letter must error too.
    let err = replace_match_string("foo", "\\x", false, false, 0, &md)
        .expect_err("\\x in replacement must be rejected");
    assert_eq!(err, "Invalid use of `\\' in replacement text");

    // A non-ASCII character must error too.
    let err = replace_match_string("foo", "\\é", false, false, 0, &md)
        .expect_err("\\<non-ascii> in replacement must be rejected");
    assert_eq!(err, "Invalid use of `\\' in replacement text");
}

// GNU's `\?` escape is the one exception to audit #12: search.c:2583
// has `else if (c != '?')` which lets `\?` fall through the
// `substart/delbackslash` branches so the bytes are copied into the
// output verbatim by the following `middle`/concat path. We mirror
// that behavior in both code paths.
#[test]
fn replace_match_passes_backslash_question_literally() {
    crate::test_utils::init_test_tracing();
    let mut md = None;
    let _ = string_match_full("foo", "foo", 0, &mut md);
    let replaced = replace_match_string("foo", "\\?", false, true, 0, &md)
        .expect("\\? must be accepted in non-literal replacement");
    // With `literal=true` the template is copied verbatim, matching
    // GNU's pass-through semantics from the other path.
    assert_eq!(replaced, "\\?");

    let replaced = replace_match_string("foo", "a\\?b", false, false, 0, &md)
        .expect("\\? must be accepted in non-literal replacement");
    assert_eq!(replaced, "a\\?b");
}

// -----------------------------------------------------------------------
// Integration: search + match data
// -----------------------------------------------------------------------

#[test]
fn search_forward_then_match_string() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut md = None;
    // Pattern with an optional group: \(foo\)\(bar\)?
    let _ = string_match_full("\\(foo\\)\\(bar\\)?", "fooXYZ", 0, &mut md);
    let md = md.as_ref().unwrap();
    assert_eq!(md.groups[1], Some((0, 3))); // "foo"
    assert_eq!(md.groups[2], None); // optional group didn't match
}

#[test]
fn string_match_start_offset_respects_real_line_start() {
    crate::test_utils::init_test_tracing();
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

#[test]
fn test_lazy_interval() {
    crate::test_utils::init_test_tracing();
    use crate::emacs_core::regex_emacs::{DefaultSyntaxLookup, search_pattern};
    let syn = DefaultSyntaxLookup;
    // Greedy: a\{1,3\} on "aaab" matches "aaa"
    let r = search_pattern("a\\{1,3\\}b", "aaab", 0, false, &syn, 0);
    let (_, regs) = r.unwrap().expect("should match");
    assert_eq!(regs.start[0], 0);
    assert_eq!(regs.end[0], 4); // matches "aaab"
}
