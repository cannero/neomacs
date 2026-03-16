use super::*;

// -- Completion matching --------------------------------------------------

#[test]
fn normalize_symbol_reader_default_uses_list_head_and_symbol_name() {
    assert_eq!(
        normalize_symbol_reader_default(Value::list(vec![
            Value::symbol("forward-char"),
            Value::symbol("backward-char"),
        ])),
        Value::string("forward-char")
    );
    assert_eq!(
        normalize_symbol_reader_default(Value::symbol("fill-column")),
        Value::string("fill-column")
    );
}

#[test]
fn normalize_buffer_reader_default_uses_list_head_and_live_buffer_name() {
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let buf_id = eval.buffers.create_buffer(" minibuffer-default ");

    assert_eq!(
        normalize_buffer_reader_default(
            &eval.buffers,
            Value::list(vec![Value::Buffer(buf_id), Value::string("fallback")]),
        ),
        Value::string(" minibuffer-default ")
    );
    assert_eq!(
        normalize_buffer_reader_default(&eval.buffers, Value::Buffer(buf_id)),
        Value::string(" minibuffer-default ")
    );
}

#[test]
fn prefix_match_basic() {
    let candidates = vec![
        "apple".into(),
        "application".into(),
        "banana".into(),
        "apply".into(),
    ];
    let result = prefix_match("app", &candidates);
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"apple".to_string()));
    assert!(result.contains(&"application".to_string()));
    assert!(result.contains(&"apply".to_string()));
}

#[test]
fn prefix_match_case_insensitive() {
    let candidates = vec!["Apple".into(), "APPLY".into(), "banana".into()];
    let result = prefix_match("app", &candidates);
    assert_eq!(result.len(), 2);
}

#[test]
fn prefix_match_empty_input() {
    let candidates = vec!["a".into(), "b".into(), "c".into()];
    let result = prefix_match("", &candidates);
    assert_eq!(result.len(), 3);
}

#[test]
fn prefix_match_no_matches() {
    let candidates = vec!["apple".into(), "banana".into()];
    let result = prefix_match("zz", &candidates);
    assert!(result.is_empty());
}

#[test]
fn substring_match_basic() {
    let candidates = vec![
        "foobar".into(),
        "bazfoo".into(),
        "hello".into(),
        "food".into(),
    ];
    let result = substring_match("foo", &candidates);
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"foobar".to_string()));
    assert!(result.contains(&"bazfoo".to_string()));
    assert!(result.contains(&"food".to_string()));
}

#[test]
fn flex_match_basic() {
    let candidates = vec![
        "find-file".into(),
        "flycheck".into(),
        "first-foo".into(),
        "hello".into(),
    ];
    // "ff" should match strings where 'f' appears twice in order.
    let result = flex_match("ff", &candidates);
    assert!(result.contains(&"find-file".to_string()));
    assert!(result.contains(&"first-foo".to_string()));
    assert!(!result.contains(&"hello".to_string()));
}

#[test]
fn flex_match_all_chars_in_order() {
    let candidates = vec!["abcdef".into(), "axbycz".into(), "zzz".into()];
    let result = flex_match("abc", &candidates);
    assert_eq!(result.len(), 2);
    assert!(result.contains(&"abcdef".to_string()));
    assert!(result.contains(&"axbycz".to_string()));
}

#[test]
fn flex_match_case_insensitive() {
    let candidates = vec!["FindFile".into()];
    let result = flex_match("ff", &candidates);
    assert_eq!(result.len(), 1);
}

#[test]
fn basic_match_case_sensitive() {
    let candidates = vec!["Apple".into(), "apple".into(), "application".into()];
    let result = basic_match("app", &candidates);
    assert_eq!(result.len(), 2);
    assert!(result.contains(&"apple".to_string()));
    assert!(result.contains(&"application".to_string()));
    assert!(!result.contains(&"Apple".to_string()));
}

// -- Common prefix --------------------------------------------------------

#[test]
fn common_prefix_empty() {
    assert!(compute_common_prefix(&[]).is_none());
}

#[test]
fn common_prefix_single() {
    let strings = vec!["hello".to_string()];
    assert_eq!(compute_common_prefix(&strings), Some("hello".to_string()));
}

#[test]
fn common_prefix_multiple() {
    let strings = vec![
        "application".to_string(),
        "apple".to_string(),
        "apply".to_string(),
    ];
    assert_eq!(compute_common_prefix(&strings), Some("appl".to_string()));
}

#[test]
fn common_prefix_no_overlap() {
    let strings = vec!["abc".to_string(), "xyz".to_string()];
    assert_eq!(compute_common_prefix(&strings), Some(String::new()));
}

#[test]
fn common_prefix_identical() {
    let strings = vec!["test".to_string(), "test".to_string()];
    assert_eq!(compute_common_prefix(&strings), Some("test".to_string()));
}

// -- History navigation ---------------------------------------------------

#[test]
fn history_navigation() {
    let mut mgr = MinibufferManager::new();
    mgr.add_to_history("test-history", "first");
    mgr.add_to_history("test-history", "second");
    mgr.add_to_history("test-history", "third");

    // Enter minibuffer with history.
    mgr.read_from_minibuffer("prompt: ", None, Some("test-history"))
        .unwrap();

    // Go back in history: should get "third" (most recent).
    let prev = mgr.history_previous();
    assert_eq!(prev, Some("third".to_string()));

    // Go back again: "second".
    let prev = mgr.history_previous();
    assert_eq!(prev, Some("second".to_string()));

    // Go forward: back to "third".
    let next = mgr.history_next();
    assert_eq!(next, Some("third".to_string()));

    // Go forward again: back to original input (empty string).
    let next = mgr.history_next();
    assert_eq!(next, Some(String::new()));

    // Go forward past the start: None.
    let next = mgr.history_next();
    assert_eq!(next, None);

    // Clean up.
    mgr.exit_minibuffer();
}

#[test]
fn history_dedup() {
    let mut mgr = MinibufferManager::new();
    mgr.add_to_history("h", "same");
    mgr.add_to_history("h", "same");
    mgr.add_to_history("h", "same");
    assert_eq!(mgr.history.get("h").len(), 1);

    mgr.add_to_history("h", "different");
    assert_eq!(mgr.history.get("h").len(), 2);
    assert_eq!(mgr.history.get("h")[0], "different");
    assert_eq!(mgr.history.get("h")[1], "same");
}

// -- Recursive minibuffer depth -------------------------------------------

#[test]
fn recursive_depth() {
    let mut mgr = MinibufferManager::new();
    assert_eq!(mgr.depth(), 0);
    assert!(!mgr.is_active());

    mgr.read_from_minibuffer("1: ", None, None).unwrap();
    assert_eq!(mgr.depth(), 1);
    assert!(mgr.is_active());

    mgr.read_from_minibuffer("2: ", None, None).unwrap();
    assert_eq!(mgr.depth(), 2);

    mgr.exit_minibuffer();
    assert_eq!(mgr.depth(), 1);

    mgr.exit_minibuffer();
    assert_eq!(mgr.depth(), 0);
    assert!(!mgr.is_active());
}

#[test]
fn recursive_depth_limit() {
    let mut mgr = MinibufferManager::new();
    mgr.max_depth = 2;

    mgr.read_from_minibuffer("1: ", None, None).unwrap();
    mgr.read_from_minibuffer("2: ", None, None).unwrap();
    let result = mgr.read_from_minibuffer("3: ", None, None);
    assert!(result.is_err());
}

#[test]
fn recursive_disabled() {
    let mut mgr = MinibufferManager::new();
    mgr.set_enable_recursive(false);

    mgr.read_from_minibuffer("1: ", None, None).unwrap();
    let result = mgr.read_from_minibuffer("2: ", None, None);
    assert!(result.is_err());
}

// -- Minibuffer enter/exit lifecycle --------------------------------------

#[test]
fn enter_exit_lifecycle() {
    let mut mgr = MinibufferManager::new();

    {
        let state = mgr
            .read_from_minibuffer("Enter: ", Some("init"), None)
            .unwrap();
        assert_eq!(state.prompt, "Enter: ");
        assert_eq!(state.content, "init");
        assert!(state.active);
        assert_eq!(state.depth, 1);
    }

    // Modify content
    {
        let state = mgr.current_mut().unwrap();
        state.content = "modified".to_string();
    }

    let result = mgr.exit_minibuffer();
    assert_eq!(result, Some("modified".to_string()));
    assert_eq!(mgr.depth(), 0);
}

#[test]
fn exit_with_default() {
    let mut mgr = MinibufferManager::new();
    {
        let state = mgr.read_from_minibuffer("Enter: ", None, None).unwrap();
        state.default_value = Some("fallback".to_string());
        // Content is empty, so default should be used.
    }
    let result = mgr.exit_minibuffer();
    assert_eq!(result, Some("fallback".to_string()));
}

#[test]
fn abort_minibuffer_clears_state() {
    let mut mgr = MinibufferManager::new();
    mgr.read_from_minibuffer("Enter: ", None, None).unwrap();
    assert_eq!(mgr.depth(), 1);
    mgr.abort_minibuffer();
    assert_eq!(mgr.depth(), 0);
    assert!(!mgr.is_active());
}

#[test]
fn exit_empty_stack() {
    let mut mgr = MinibufferManager::new();
    assert_eq!(mgr.exit_minibuffer(), None);
}

// -- MinibufferManager completion -----------------------------------------

#[test]
fn try_complete_with_table() {
    let mut mgr = MinibufferManager::new();
    {
        let state = mgr
            .read_from_minibuffer("M-x ", Some("find"), None)
            .unwrap();
        state.completion_table = Some(CompletionTable::List(vec![
            "find-file".into(),
            "find-file-other-window".into(),
            "find-tag".into(),
            "forward-char".into(),
        ]));
    }
    let state = mgr.current().unwrap();
    let result = mgr.try_complete(state);
    assert_eq!(result.matches.len(), 3); // find-file, find-file-other-window, find-tag
    assert_eq!(result.common_prefix, Some("find-".to_string()));
    mgr.exit_minibuffer();
}

#[test]
fn test_completion_exact_match() {
    let mgr = MinibufferManager::new();
    let table = CompletionTable::List(vec!["apple".into(), "banana".into(), "cherry".into()]);
    assert!(mgr.test_completion("apple", &table));
    assert!(mgr.test_completion("banana", &table));
    assert!(!mgr.test_completion("app", &table));
    assert!(!mgr.test_completion("APPLE", &table));
}

#[test]
fn try_completion_string_result() {
    let mgr = MinibufferManager::new();
    let table = CompletionTable::List(vec!["application".into(), "apple".into(), "apply".into()]);
    let result = mgr.try_completion_string("app", &table);
    assert_eq!(result, Some("appl".to_string()));
}

#[test]
fn all_completions_empty() {
    let mgr = MinibufferManager::new();
    let table = CompletionTable::List(vec!["foo".into(), "bar".into()]);
    let result = mgr.all_completions("zzz", &table);
    assert!(result.is_empty());
}

// -- Completion with different styles -------------------------------------

#[test]
fn completion_style_substring() {
    let mut mgr = MinibufferManager::new();
    mgr.set_completion_style(CompletionStyle::Substring);
    let table = CompletionTable::List(vec![
        "find-file".into(),
        "describe-file".into(),
        "file-name".into(),
    ]);
    let result = mgr.all_completions("file", &table);
    assert_eq!(result.len(), 3); // All contain "file"
}

#[test]
fn completion_style_flex() {
    let mut mgr = MinibufferManager::new();
    mgr.set_completion_style(CompletionStyle::Flex);
    let table = CompletionTable::List(vec![
        "find-file".into(),
        "forward-char".into(),
        "flycheck".into(),
    ]);
    // "ff" should flex-match "find-file" and "flycheck" (f...f? no, flycheck has no second f)
    // Actually: "find-file" has f...f, "flycheck" has f but only one f total.
    let result = mgr.all_completions("ff", &table);
    assert!(result.contains(&"find-file".to_string()));
    // "flycheck" has only one 'f', so "ff" won't match it.
    assert!(!result.contains(&"flycheck".to_string()));
}

#[test]
fn completion_style_basic_case_sensitive() {
    let mut mgr = MinibufferManager::new();
    mgr.set_completion_style(CompletionStyle::Basic);
    let table = CompletionTable::List(vec!["Apple".into(), "apple".into(), "application".into()]);
    let result = mgr.all_completions("app", &table);
    assert_eq!(result.len(), 2);
    assert!(result.contains(&"apple".to_string()));
    assert!(result.contains(&"application".to_string()));
}

// -- Alist completion table -----------------------------------------------

#[test]
fn alist_completion() {
    let mgr = MinibufferManager::new();
    let table = CompletionTable::Alist(vec![
        ("alpha".into(), Value::Int(1)),
        ("beta".into(), Value::Int(2)),
        ("alphabetical".into(), Value::Int(3)),
    ]);
    let result = mgr.all_completions("alph", &table);
    assert_eq!(result.len(), 2);
}

#[test]
fn builtin_try_completion_unique_exact() {
    // Exact unique match should return t.
    let coll = Value::list(vec![Value::string("unique"), Value::string("other")]);
    let result = builtin_try_completion(vec![Value::string("unique"), coll]).unwrap();
    assert!(matches!(result, Value::True));
}

#[test]
fn builtin_try_completion_common_prefix() {
    let coll = Value::list(vec![Value::string("application"), Value::string("apple")]);
    let result = builtin_try_completion(vec![Value::string("app"), coll]).unwrap();
    assert!(result.as_str().unwrap() == "appl");
}

#[test]
fn builtin_try_completion_no_match() {
    let coll = Value::list(vec![Value::string("foo"), Value::string("bar")]);
    let result = builtin_try_completion(vec![Value::string("zzz"), coll]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn builtin_try_completion_rejects_more_than_three_args() {
    let coll = Value::list(vec![Value::string("a")]);
    let result = builtin_try_completion(vec![Value::string(""), coll, Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_all_completions_returns_list() {
    let coll = Value::list(vec![
        Value::string("apple"),
        Value::string("application"),
        Value::string("banana"),
    ]);
    let result = builtin_all_completions(vec![Value::string("app"), coll]).unwrap();
    let items = super::super::value::list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn builtin_all_completions_rejects_more_than_four_args() {
    let coll = Value::list(vec![Value::string("a")]);
    let result = builtin_all_completions(vec![
        Value::string(""),
        coll,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_test_completion_match() {
    let coll = Value::list(vec![Value::string("alpha"), Value::string("beta")]);
    let result = builtin_test_completion(vec![Value::string("alpha"), coll]).unwrap();
    assert!(matches!(result, Value::True));
}

#[test]
fn builtin_test_completion_no_match() {
    let coll = Value::list(vec![Value::string("alpha"), Value::string("beta")]);
    let result = builtin_test_completion(vec![Value::string("alp"), coll]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn builtin_test_completion_rejects_more_than_three_args() {
    let coll = Value::list(vec![Value::string("a")]);
    let result = builtin_test_completion(vec![Value::string(""), coll, Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_minibuffer_depth_returns_zero() {
    let result = builtin_minibuffer_depth(vec![]).unwrap();
    assert!(matches!(result, Value::Int(0)));
}

#[test]
fn builtin_minibufferp_returns_nil() {
    let result = builtin_minibufferp(vec![]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn builtin_minibufferp_accepts_string_and_second_arg() {
    let result = builtin_minibufferp(vec![Value::string("x"), Value::Nil]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn builtin_minibufferp_rejects_non_buffer_like_values() {
    let result = builtin_minibufferp(vec![Value::Int(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn builtin_minibufferp_rejects_more_than_two_args() {
    let result = builtin_minibufferp(vec![Value::Nil, Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_recursive_edit_returns_nil() {
    let result = builtin_recursive_edit(vec![]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn builtin_recursive_edit_rejects_args() {
    let result = builtin_recursive_edit(vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_top_level_throws_top_level_tag() {
    let result = builtin_top_level(vec![]);
    // top-level now throws 'top-level to exit all recursive edits
    // (mirrors GNU Emacs keyboard.c:1187 Ftop_level).
    assert!(matches!(
        result,
        Err(Flow::Throw { tag, value })
            if matches!(tag, Value::Symbol(ref id) if resolve_sym(*id) == "top-level") && value.is_nil()
    ));
}

#[test]
fn builtin_top_level_rejects_args() {
    let result = builtin_top_level(vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_exit_recursive_edit_signals_user_error() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_exit_recursive_edit(&mut eval, vec![]);
    // Not in a recursive edit → user-error
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "user-error"
    ));
}

#[test]
fn builtin_exit_recursive_edit_rejects_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_exit_recursive_edit(&mut eval, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_minibuffer_contents_returns_current_buffer_text() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.buffers
        .current_buffer_mut()
        .expect("scratch buffer")
        .insert("probe");
    let result = builtin_minibuffer_contents(&mut eval, vec![]).unwrap();
    assert!(result.as_str().unwrap() == "probe");
}

#[test]
fn builtin_minibuffer_contents_no_properties_returns_current_buffer_text() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.buffers
        .current_buffer_mut()
        .expect("scratch buffer")
        .insert("probe");
    let result = builtin_minibuffer_contents_no_properties(&mut eval, vec![]).unwrap();
    assert!(result.as_str().unwrap() == "probe");
}

#[test]
fn builtin_minibuffer_contents_no_properties_rejects_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_minibuffer_contents_no_properties(&mut eval, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_exit_minibuffer_throws_exit_tag() {
    let result = builtin_exit_minibuffer(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Throw { tag, value })
            if matches!(tag, Value::Symbol(ref id) if resolve_sym(*id) == "exit") && value.is_nil()
    ));
}

#[test]
fn builtin_abort_minibuffers_signals_not_in_minibuffer_error() {
    let result = builtin_abort_minibuffers(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && matches!(sig.data.as_slice(), [val] if val.as_str().map(|s| s == "Not in a minibuffer").unwrap_or(false))
    ));
}

#[test]
fn builtin_abort_minibuffers_rejects_args() {
    let result = builtin_abort_minibuffers(vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_abort_recursive_edit_signals_user_error() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_abort_recursive_edit(&mut eval, vec![]);
    // Not in a recursive edit → user-error
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "user-error"
    ));
}

#[test]
fn builtin_abort_recursive_edit_rejects_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_abort_recursive_edit(&mut eval, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_read_file_name_signals_end_of_file() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_file_name(
        &mut eval,
        vec![
            Value::string("File: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::string("/tmp/test.txt"),
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "end-of-file"
                && matches!(sig.data.as_slice(), [val] if val.as_str().map(|s| s == "Error reading from stdin").unwrap_or(false))
    ));
}

#[test]
fn builtin_read_file_name_validates_dir_default_and_initial() {
    let mut eval = super::super::eval::Evaluator::new();
    let bad_dir = builtin_read_file_name(&mut eval, vec![Value::string("File: "), Value::Int(1)]);
    assert!(matches!(
        bad_dir,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));

    let bad_default = builtin_read_file_name(
        &mut eval,
        vec![Value::string("File: "), Value::Nil, Value::Int(1)],
    );
    assert!(matches!(
        bad_default,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));

    let bad_initial = builtin_read_file_name(
        &mut eval,
        vec![
            Value::string("File: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(1),
        ],
    );
    assert!(matches!(
        bad_initial,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn builtin_read_file_name_rejects_more_than_six_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_file_name(
        &mut eval,
        vec![
            Value::string("File: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_read_buffer_signals_end_of_file() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_buffer(
        &mut eval,
        vec![Value::string("Buffer: "), Value::string("*scratch*")],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
}

#[test]
fn builtin_read_directory_name_rejects_more_than_five_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_directory_name(
        &mut eval,
        vec![
            Value::string("Directory: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_read_directory_name_validates_dir_default_and_initial() {
    let mut eval = super::super::eval::Evaluator::new();
    let bad_dir =
        builtin_read_directory_name(&mut eval, vec![Value::string("Directory: "), Value::Int(1)]);
    assert!(matches!(
        bad_dir,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));

    let bad_default = builtin_read_directory_name(
        &mut eval,
        vec![Value::string("Directory: "), Value::Nil, Value::Int(1)],
    );
    assert!(matches!(
        bad_default,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));

    let bad_initial = builtin_read_directory_name(
        &mut eval,
        vec![
            Value::string("Directory: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(1),
        ],
    );
    assert!(matches!(
        bad_initial,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn builtin_read_buffer_rejects_more_than_four_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_buffer(
        &mut eval,
        vec![
            Value::string("Buffer: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_read_command_rejects_more_than_two_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_command(
        &mut eval,
        vec![Value::string("Command: "), Value::Nil, Value::Nil],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn builtin_read_variable_rejects_more_than_two_args() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_read_variable(
        &mut eval,
        vec![Value::string("Variable: "), Value::Nil, Value::Nil],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

// -- value_to_string_list -------------------------------------------------

#[test]
fn value_to_string_list_from_list() {
    let list = Value::list(vec![
        Value::string("foo"),
        Value::string("bar"),
        Value::string("baz"),
    ]);
    let result = value_to_string_list(&list);
    assert_eq!(result, vec!["foo", "bar", "baz"]);
}

#[test]
fn value_to_string_list_from_alist() {
    let alist = Value::list(vec![
        Value::cons(Value::string("key1"), Value::Int(1)),
        Value::cons(Value::string("key2"), Value::Int(2)),
    ]);
    let result = value_to_string_list(&alist);
    assert_eq!(result, vec!["key1", "key2"]);
}

#[test]
fn value_to_string_list_from_nil() {
    let result = value_to_string_list(&Value::Nil);
    assert!(result.is_empty());
}

#[test]
fn value_to_string_list_from_vector() {
    let vec = Value::vector(vec![Value::string("a"), Value::string("b")]);
    let result = value_to_string_list(&vec);
    assert_eq!(result, vec!["a", "b"]);
}
