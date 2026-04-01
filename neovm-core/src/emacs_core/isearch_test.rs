use super::*;

// -----------------------------------------------------------------------
// SearchHistory
// -----------------------------------------------------------------------

#[test]
fn history_push_and_get() {
    crate::test_utils::init_test_tracing();
    let mut h = SearchHistory::new();
    h.push("hello".to_string(), false);
    h.push("world".to_string(), false);
    assert_eq!(h.get(0, false), Some("world"));
    assert_eq!(h.get(1, false), Some("hello"));
    assert_eq!(h.get(2, false), None);
}

#[test]
fn history_push_deduplicates() {
    crate::test_utils::init_test_tracing();
    let mut h = SearchHistory::new();
    h.push("aaa".to_string(), false);
    h.push("bbb".to_string(), false);
    h.push("aaa".to_string(), false);
    assert_eq!(h.len(false), 2);
    assert_eq!(h.get(0, false), Some("aaa"));
    assert_eq!(h.get(1, false), Some("bbb"));
}

#[test]
fn history_separate_rings() {
    crate::test_utils::init_test_tracing();
    let mut h = SearchHistory::new();
    h.push("literal".to_string(), false);
    h.push("re.*gex".to_string(), true);
    assert_eq!(h.len(false), 1);
    assert_eq!(h.len(true), 1);
    assert_eq!(h.get(0, false), Some("literal"));
    assert_eq!(h.get(0, true), Some("re.*gex"));
}

#[test]
fn history_max_length() {
    crate::test_utils::init_test_tracing();
    let mut h = SearchHistory::new();
    for i in 0..150 {
        h.push(format!("item{}", i), false);
    }
    assert_eq!(h.len(false), 100);
    // Most recent is item149
    assert_eq!(h.get(0, false), Some("item149"));
}

#[test]
fn history_strings_accessor() {
    crate::test_utils::init_test_tracing();
    let mut h = SearchHistory::new();
    h.push("a".to_string(), false);
    h.push("b".to_string(), false);
    let ring = h.strings(false);
    assert_eq!(ring.len(), 2);
    assert_eq!(ring[0], "b");
    assert_eq!(ring[1], "a");
}

// -----------------------------------------------------------------------
// IsearchManager — begin/end lifecycle
// -----------------------------------------------------------------------

#[test]
fn isearch_begin_end_lifecycle() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    assert!(!mgr.is_active());

    mgr.begin_search(SearchDirection::Forward, false, 42);
    assert!(mgr.is_active());

    let state = mgr.state().unwrap();
    assert_eq!(state.origin, 42);
    assert!(state.search_string.is_empty());
    assert!(matches!(state.direction, SearchDirection::Forward));
    assert!(!state.regexp);

    mgr.end_search(false);
    assert!(!mgr.is_active());
}

#[test]
fn isearch_abort_restores_origin() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 100);
    mgr.add_char('x');
    let origin = mgr.abort_search();
    assert_eq!(origin, 100);
    assert!(!mgr.is_active());
}

#[test]
fn isearch_end_saves_to_history() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.add_char('f');
    mgr.add_char('o');
    mgr.add_char('o');
    mgr.end_search(true);
    assert_eq!(mgr.history.get(0, false), Some("foo"));
}

#[test]
fn isearch_end_empty_string_not_saved() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.end_search(true);
    assert_eq!(mgr.history.len(false), 0);
}

// -----------------------------------------------------------------------
// IsearchManager — string modification
// -----------------------------------------------------------------------

#[test]
fn isearch_add_delete_char() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.add_char('a');
    mgr.add_char('b');
    mgr.add_char('c');
    assert_eq!(mgr.state().unwrap().search_string, "abc");
    mgr.delete_char();
    assert_eq!(mgr.state().unwrap().search_string, "ab");
}

#[test]
fn isearch_set_string() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("hello world".to_string());
    assert_eq!(mgr.state().unwrap().search_string, "hello world");
}

#[test]
fn isearch_toggle_regexp() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    assert!(!mgr.state().unwrap().regexp);
    mgr.toggle_regexp();
    assert!(mgr.state().unwrap().regexp);
    mgr.toggle_regexp();
    assert!(!mgr.state().unwrap().regexp);
}

#[test]
fn isearch_toggle_case_fold() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    assert!(mgr.state().unwrap().case_fold.is_none());
    mgr.toggle_case_fold();
    assert_eq!(mgr.state().unwrap().case_fold, Some(true));
    mgr.toggle_case_fold();
    assert_eq!(mgr.state().unwrap().case_fold, Some(false));
    mgr.toggle_case_fold();
    assert!(mgr.state().unwrap().case_fold.is_none());
}

#[test]
fn isearch_reverse_direction() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    assert!(matches!(
        mgr.state().unwrap().direction,
        SearchDirection::Forward
    ));
    mgr.reverse_direction();
    assert!(matches!(
        mgr.state().unwrap().direction,
        SearchDirection::Backward
    ));
}

// -----------------------------------------------------------------------
// IsearchManager — forward matching
// -----------------------------------------------------------------------

#[test]
fn isearch_forward_search_update_finds_match() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hello world hello";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("world".to_string());
    let result = mgr.search_update(text);
    assert_eq!(result, Some((6, 11)));
    assert!(mgr.state().unwrap().success);
}

#[test]
fn isearch_forward_search_update_no_match() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hello world";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("zzz".to_string());
    let result = mgr.search_update(text);
    assert!(result.is_none());
    assert!(!mgr.state().unwrap().success);
}

#[test]
fn isearch_forward_search_next_advances() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb aaa bbb";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("aaa".to_string());

    // First search_update finds first occurrence
    let r1 = mgr.search_update(text);
    assert_eq!(r1, Some((0, 3)));

    // search_next moves to next occurrence
    let r2 = mgr.search_next(text);
    assert_eq!(r2, Some((8, 11)));
}

// -----------------------------------------------------------------------
// IsearchManager — backward matching
// -----------------------------------------------------------------------

#[test]
fn isearch_backward_search_update() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb aaa";
    mgr.begin_search(SearchDirection::Backward, false, text.len());
    mgr.set_string("aaa".to_string());
    let result = mgr.search_update(text);
    // Backward from origin=11, should find last "aaa" at 8
    assert_eq!(result, Some((8, 11)));
}

#[test]
fn isearch_backward_search_next() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb aaa";
    mgr.begin_search(SearchDirection::Backward, false, text.len());
    mgr.set_string("aaa".to_string());

    let r1 = mgr.search_update(text);
    assert_eq!(r1, Some((8, 11)));

    // search_next backward should find the earlier "aaa"
    let r2 = mgr.search_next(text);
    assert_eq!(r2, Some((0, 3)));
}

// -----------------------------------------------------------------------
// IsearchManager — wrapping
// -----------------------------------------------------------------------

#[test]
fn isearch_forward_wraps() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb ccc";
    // Start near the end so "aaa" is behind us
    mgr.begin_search(SearchDirection::Forward, false, 8);
    mgr.set_string("aaa".to_string());

    // search_update: forward from origin=8, no "aaa" after 8, wraps to find at 0
    let result = mgr.search_update(text);
    assert_eq!(result, Some((0, 3)));
    assert!(mgr.state().unwrap().wrapped);
}

#[test]
fn isearch_backward_wraps() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb ccc";
    // Start at the beginning so "ccc" is ahead of us
    mgr.begin_search(SearchDirection::Backward, false, 0);
    mgr.set_string("ccc".to_string());

    let result = mgr.search_update(text);
    assert_eq!(result, Some((8, 11)));
    assert!(mgr.state().unwrap().wrapped);
}

// -----------------------------------------------------------------------
// IsearchManager — case fold auto-detection
// -----------------------------------------------------------------------

#[test]
fn case_fold_auto_lowercase_folds() {
    crate::test_utils::init_test_tracing();
    assert!(resolve_case_fold(None, "hello"));
}

#[test]
fn case_fold_auto_uppercase_exact() {
    crate::test_utils::init_test_tracing();
    assert!(!resolve_case_fold(None, "Hello"));
}

#[test]
fn case_fold_override_true() {
    crate::test_utils::init_test_tracing();
    assert!(resolve_case_fold(Some(true), "Hello"));
}

#[test]
fn case_fold_override_false() {
    crate::test_utils::init_test_tracing();
    assert!(!resolve_case_fold(Some(false), "hello"));
}

#[test]
fn isearch_case_fold_auto() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "Hello World hello world";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    // Lowercase search string — should auto-fold
    mgr.set_string("hello".to_string());
    let result = mgr.search_update(text);
    // Should find "Hello" at 0 (case-folded)
    assert_eq!(result, Some((0, 5)));
}

#[test]
fn isearch_case_fold_auto_uppercase_exact() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hello world Hello World";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    // Uppercase letter in search — should NOT fold
    mgr.set_string("Hello".to_string());
    let result = mgr.search_update(text);
    assert_eq!(result, Some((12, 17)));
}

// -----------------------------------------------------------------------
// IsearchManager — regexp search
// -----------------------------------------------------------------------

#[test]
fn isearch_regexp_forward() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "foo 123 bar 456";
    mgr.begin_search(SearchDirection::Forward, true, 0);
    mgr.set_string("[0-9]+".to_string());
    let result = mgr.search_update(text);
    assert_eq!(result, Some((4, 7)));
}

#[test]
fn isearch_regexp_backward() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "foo 123 bar 456";
    mgr.begin_search(SearchDirection::Backward, true, text.len());
    mgr.set_string("[0-9]+".to_string());
    let result = mgr.search_update(text);
    assert_eq!(result, Some((12, 15)));
}

// -----------------------------------------------------------------------
// IsearchManager — lazy matches
// -----------------------------------------------------------------------

#[test]
fn compute_lazy_matches_literal() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aa bb aa cc aa";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("aa".to_string());
    mgr.compute_lazy_matches(text, 0, text.len());
    let matches = &mgr.state().unwrap().lazy_matches;
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0], (0, 2));
    assert_eq!(matches[1], (6, 8));
    assert_eq!(matches[2], (12, 14));
}

#[test]
fn compute_lazy_matches_regexp() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "abc 123 def 456 ghi";
    mgr.begin_search(SearchDirection::Forward, true, 0);
    mgr.set_string("[0-9]+".to_string());
    mgr.compute_lazy_matches(text, 0, text.len());
    let matches = &mgr.state().unwrap().lazy_matches;
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0], (4, 7));
    assert_eq!(matches[1], (12, 15));
}

#[test]
fn compute_lazy_matches_visible_region() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "aaa bbb aaa ccc aaa";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("aaa".to_string());
    // Only look in the middle region [4..15]
    mgr.compute_lazy_matches(text, 4, 15);
    let matches = &mgr.state().unwrap().lazy_matches;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0], (8, 11));
}

#[test]
fn compute_lazy_matches_empty_string() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hello world";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string(String::new());
    mgr.compute_lazy_matches(text, 0, text.len());
    assert!(mgr.state().unwrap().lazy_matches.is_empty());
}

// -----------------------------------------------------------------------
// IsearchManager — history navigation
// -----------------------------------------------------------------------

#[test]
fn isearch_history_navigation() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();

    // Populate history
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("first".to_string());
    mgr.end_search(true);

    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("second".to_string());
    mgr.end_search(true);

    // Start a new search and navigate history
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.history_previous();
    assert_eq!(mgr.state().unwrap().search_string, "second");
    mgr.history_previous();
    assert_eq!(mgr.state().unwrap().search_string, "first");
    mgr.history_next();
    assert_eq!(mgr.state().unwrap().search_string, "second");
    mgr.history_next();
    assert!(mgr.state().unwrap().search_string.is_empty());
}

// -----------------------------------------------------------------------
// IsearchManager — yank_word_or_char
// -----------------------------------------------------------------------

#[test]
fn isearch_yank_word() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hello world";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.yank_word_or_char(text, 0);
    assert_eq!(mgr.state().unwrap().search_string, "hello");
}

#[test]
fn isearch_yank_nonword_char() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = " hello";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.yank_word_or_char(text, 0);
    // Space is not alphanumeric, so only one char yanked
    assert_eq!(mgr.state().unwrap().search_string, " ");
}

#[test]
fn isearch_yank_at_end() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    let text = "hi";
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.yank_word_or_char(text, 2);
    // At end of text, nothing yanked
    assert!(mgr.state().unwrap().search_string.is_empty());
}

// -----------------------------------------------------------------------
// IsearchManager — prompt
// -----------------------------------------------------------------------

#[test]
fn isearch_prompt_basic() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Forward, false, 0);
    mgr.set_string("test".to_string());
    let prompt = mgr.prompt();
    assert!(prompt.contains("I-search"));
    assert!(prompt.contains("test"));
    assert!(!prompt.contains("Regexp"));
}

#[test]
fn isearch_prompt_regexp_backward_failing() {
    crate::test_utils::init_test_tracing();
    let mut mgr = IsearchManager::new();
    mgr.begin_search(SearchDirection::Backward, true, 0);
    mgr.set_string("pat".to_string());
    // Force a failing search
    let _ = mgr.search_update("no match here");
    let prompt = mgr.prompt();
    assert!(prompt.contains("Failing"));
    assert!(prompt.contains("Regexp"));
    assert!(prompt.contains("I-search backward"));
}

// -----------------------------------------------------------------------
// QueryReplaceManager — lifecycle
// -----------------------------------------------------------------------

#[test]
fn query_replace_begin_end() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    assert!(!mgr.is_active());
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    assert!(mgr.is_active());
    let state = mgr.state().unwrap();
    assert_eq!(state.from_string, "foo");
    assert_eq!(state.to_string, "bar");
    assert!(!state.regexp);
    assert!(state.region_start.is_none());

    let summary = mgr.finish();
    assert_eq!(summary.replaced, 0);
    assert_eq!(summary.skipped, 0);
    assert!(!mgr.is_active());
}

#[test]
fn query_replace_begin_in_region() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin_in_region("x".to_string(), "y".to_string(), false, 10, 50);
    let state = mgr.state().unwrap();
    assert_eq!(state.region_start, Some(10));
    assert_eq!(state.region_end, Some(50));
}

// -----------------------------------------------------------------------
// QueryReplaceManager — find_next
// -----------------------------------------------------------------------

#[test]
fn query_replace_find_next_basic() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar foo baz foo";
    mgr.begin("foo".to_string(), "qux".to_string(), false);

    let r1 = mgr.find_next(text, 0);
    assert_eq!(r1, Some((0, 3)));

    let r2 = mgr.find_next(text, 3);
    assert_eq!(r2, Some((8, 11)));

    let r3 = mgr.find_next(text, 11);
    assert_eq!(r3, Some((16, 19)));

    let r4 = mgr.find_next(text, 19);
    assert!(r4.is_none());
}

#[test]
fn query_replace_find_next_in_region() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar foo baz foo";
    mgr.begin_in_region("foo".to_string(), "qux".to_string(), false, 4, 15);

    let r1 = mgr.find_next(text, 0);
    assert_eq!(r1, Some((8, 11)));

    // Next "foo" is at 16 which is beyond region_end=15
    let r2 = mgr.find_next(text, 11);
    assert!(r2.is_none());
}

#[test]
fn query_replace_find_next_regexp() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "abc 123 def 456";
    mgr.begin("[0-9]+".to_string(), "NUM".to_string(), true);

    let r1 = mgr.find_next(text, 0);
    assert_eq!(r1, Some((4, 7)));

    let r2 = mgr.find_next(text, 7);
    assert_eq!(r2, Some((12, 15)));
}

// -----------------------------------------------------------------------
// QueryReplaceManager — respond
// -----------------------------------------------------------------------

#[test]
fn query_replace_respond_yes() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar";
    mgr.begin("foo".to_string(), "baz".to_string(), false);
    let _ = mgr.find_next(text, 0);

    let action = mgr.respond(QueryReplaceResponse::Yes);
    match action {
        QueryReplaceAction::Replace(start, end, repl) => {
            assert_eq!(start, 0);
            assert_eq!(end, 3);
            assert_eq!(repl, "baz");
        }
        _ => panic!("expected Replace action"),
    }
    assert_eq!(mgr.state().unwrap().replaced_count, 1);
}

#[test]
fn query_replace_respond_no() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar";
    mgr.begin("foo".to_string(), "baz".to_string(), false);
    let _ = mgr.find_next(text, 0);

    let action = mgr.respond(QueryReplaceResponse::No);
    assert!(matches!(action, QueryReplaceAction::Skip));
    assert_eq!(mgr.state().unwrap().skipped_count, 1);
}

#[test]
fn query_replace_respond_quit() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar foo";
    mgr.begin("foo".to_string(), "baz".to_string(), false);
    let _ = mgr.find_next(text, 0);
    mgr.respond(QueryReplaceResponse::Yes);

    let _ = mgr.find_next(text, 3);
    let action = mgr.respond(QueryReplaceResponse::Quit);
    match action {
        QueryReplaceAction::Done(summary) => {
            assert_eq!(summary.replaced, 1);
            assert_eq!(summary.skipped, 0);
        }
        _ => panic!("expected Done action"),
    }
    assert!(!mgr.is_active());
}

#[test]
fn query_replace_respond_delete() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar";
    mgr.begin("foo".to_string(), "baz".to_string(), false);
    let _ = mgr.find_next(text, 0);

    let action = mgr.respond(QueryReplaceResponse::Delete);
    match action {
        QueryReplaceAction::Replace(start, end, repl) => {
            assert_eq!(start, 0);
            assert_eq!(end, 3);
            assert!(repl.is_empty());
        }
        _ => panic!("expected Replace with empty string"),
    }
}

#[test]
fn query_replace_respond_help() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);

    let action = mgr.respond(QueryReplaceResponse::Help);
    match action {
        QueryReplaceAction::ShowHelp(text) => {
            assert!(text.contains("replace this match"));
            assert!(text.contains("skip this match"));
        }
        _ => panic!("expected ShowHelp action"),
    }
}

#[test]
fn query_replace_respond_edit() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    let action = mgr.respond(QueryReplaceResponse::Edit);
    assert!(matches!(action, QueryReplaceAction::NeedInput));
}

// -----------------------------------------------------------------------
// QueryReplaceManager — undo
// -----------------------------------------------------------------------

#[test]
fn query_replace_undo() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    let text = "foo bar foo";
    mgr.begin("foo".to_string(), "baz".to_string(), false);

    let _ = mgr.find_next(text, 0);
    mgr.respond(QueryReplaceResponse::Yes);
    assert_eq!(mgr.state().unwrap().replaced_count, 1);

    let undo = mgr.undo_last();
    assert!(undo.is_some());
    let undo = undo.unwrap();
    assert_eq!(undo.position, 0);
    assert_eq!(undo.replacement, "baz");
    assert_eq!(mgr.state().unwrap().replaced_count, 0);
}

#[test]
fn query_replace_undo_empty_stack() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    assert!(mgr.undo_last().is_none());
}

// -----------------------------------------------------------------------
// QueryReplaceManager — compute_replacement with case preservation
// -----------------------------------------------------------------------

#[test]
fn query_replace_compute_replacement_lower() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    assert_eq!(mgr.compute_replacement("foo"), "bar");
}

#[test]
fn query_replace_compute_replacement_upper() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    assert_eq!(mgr.compute_replacement("FOO"), "BAR");
}

#[test]
fn query_replace_compute_replacement_capitalized() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("foo".to_string(), "bar".to_string(), false);
    assert_eq!(mgr.compute_replacement("Foo"), "Bar");
}

// -----------------------------------------------------------------------
// QueryReplaceManager — prompt
// -----------------------------------------------------------------------

#[test]
fn query_replace_prompt() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("old".to_string(), "new".to_string(), false);
    let prompt = mgr.prompt();
    assert!(prompt.contains("Query replacing"));
    assert!(prompt.contains("old"));
    assert!(prompt.contains("new"));
    assert!(!prompt.contains("regexp"));
}

#[test]
fn query_replace_prompt_regexp() {
    crate::test_utils::init_test_tracing();
    let mut mgr = QueryReplaceManager::new();
    mgr.begin("[0-9]+".to_string(), "NUM".to_string(), true);
    let prompt = mgr.prompt();
    assert!(prompt.contains("regexp"));
}

// -----------------------------------------------------------------------
// preserve_case helper
// -----------------------------------------------------------------------

#[test]
fn preserve_case_all_lower() {
    crate::test_utils::init_test_tracing();
    assert_eq!(preserve_case("bar", "foo"), "bar");
}

#[test]
fn preserve_case_all_upper() {
    crate::test_utils::init_test_tracing();
    assert_eq!(preserve_case("bar", "FOO"), "BAR");
}

#[test]
fn preserve_case_capitalized() {
    crate::test_utils::init_test_tracing();
    assert_eq!(preserve_case("bar", "Foo"), "Bar");
}

#[test]
fn preserve_case_mixed() {
    crate::test_utils::init_test_tracing();
    // Mixed case like "fOo" doesn't match any pattern, return as-is
    assert_eq!(preserve_case("bar", "fOo"), "bar");
}

#[test]
fn preserve_case_empty_matched() {
    crate::test_utils::init_test_tracing();
    assert_eq!(preserve_case("bar", ""), "bar");
}

#[test]
fn preserve_case_empty_replacement() {
    crate::test_utils::init_test_tracing();
    assert_eq!(preserve_case("", "FOO"), "");
}

#[test]
fn preserve_case_non_alpha_upper() {
    crate::test_utils::init_test_tracing();
    // "123" has no alphabetic chars, so all_upper && has_alpha is false
    assert_eq!(preserve_case("bar", "123"), "bar");
}

// -----------------------------------------------------------------------
// find_match helper
// -----------------------------------------------------------------------

#[test]
fn find_match_literal_forward() {
    crate::test_utils::init_test_tracing();
    let text = "hello world hello";
    let result = find_match(text, "hello", 0, true, false, false);
    assert_eq!(result, Some((0, 5)));
}

#[test]
fn find_match_literal_forward_from_offset() {
    crate::test_utils::init_test_tracing();
    let text = "hello world hello";
    let result = find_match(text, "hello", 1, true, false, false);
    assert_eq!(result, Some((12, 17)));
}

#[test]
fn find_match_literal_backward() {
    crate::test_utils::init_test_tracing();
    let text = "hello world hello";
    let result = find_match(text, "hello", text.len(), false, false, false);
    assert_eq!(result, Some((12, 17)));
}

#[test]
fn find_match_literal_backward_from_middle() {
    crate::test_utils::init_test_tracing();
    let text = "hello world hello";
    let result = find_match(text, "hello", 10, false, false, false);
    assert_eq!(result, Some((0, 5)));
}

#[test]
fn find_match_case_fold() {
    crate::test_utils::init_test_tracing();
    let text = "Hello World";
    let result = find_match(text, "hello", 0, true, false, true);
    assert_eq!(result, Some((0, 5)));
}

#[test]
fn find_match_case_sensitive() {
    crate::test_utils::init_test_tracing();
    let text = "Hello World";
    let result = find_match(text, "hello", 0, true, false, false);
    assert!(result.is_none());
}

#[test]
fn find_match_regexp_forward() {
    crate::test_utils::init_test_tracing();
    let text = "foo 123 bar";
    let result = find_match(text, "[0-9]+", 0, true, true, false);
    assert_eq!(result, Some((4, 7)));
}

#[test]
fn find_match_regexp_backward() {
    crate::test_utils::init_test_tracing();
    let text = "foo 123 bar 456";
    let result = find_match(text, "[0-9]+", text.len(), false, true, false);
    assert_eq!(result, Some((12, 15)));
}

#[test]
fn find_match_empty_pattern() {
    crate::test_utils::init_test_tracing();
    let text = "hello";
    assert!(find_match(text, "", 0, true, false, false).is_none());
}

#[test]
fn find_match_no_match() {
    crate::test_utils::init_test_tracing();
    let text = "hello world";
    assert!(find_match(text, "zzz", 0, true, false, false).is_none());
}

#[test]
fn find_match_at_boundary() {
    crate::test_utils::init_test_tracing();
    let text = "abcdef";
    let result = find_match(text, "def", 3, true, false, false);
    assert_eq!(result, Some((3, 6)));
}

#[test]
fn delimited_match_rejects_embedded_word() {
    crate::test_utils::init_test_tracing();
    let text = "foo1 1foo foo";
    assert!(!is_delimited_match(text, 0, 3));
    assert!(!is_delimited_match(text, 5, 8));
    assert!(is_delimited_match(text, 10, 13));
}

#[test]
fn delimited_match_treats_underscore_as_delimiter() {
    crate::test_utils::init_test_tracing();
    let text = "foo_foo";
    assert!(is_delimited_match(text, 0, 3));
    assert!(is_delimited_match(text, 4, 7));
}

// -----------------------------------------------------------------------
// Builtin function stubs
// -----------------------------------------------------------------------

#[test]
fn builtin_isearch_forward_signals_batch_buffer_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_isearch_forward(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && sig.data == vec![Value::string("move-to-window-line called from unrelated buffer")]
    ));
}

#[test]
fn builtin_isearch_backward_signals_batch_buffer_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_isearch_backward(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && sig.data == vec![Value::string("move-to-window-line called from unrelated buffer")]
    ));
}

#[test]
fn builtin_isearch_forward_rejects_too_many_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_isearch_forward(vec![Value::NIL, Value::NIL, Value::NIL]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_isearch_backward_rejects_too_many_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_isearch_backward(vec![Value::NIL, Value::NIL, Value::NIL]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}
