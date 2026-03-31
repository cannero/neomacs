use super::*;
use crate::emacs_core::eval::Context;

/// Test helper: create a fresh eval context for locate-file tests.
fn test_eval_ctx() -> Context {
    Context::new()
}

#[test]
fn eval_buffer_evaluates_current_buffer_forms() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("(setq lread-eb-a 11)\n(setq lread-eb-b (+ lread-eb-a 1))");
    }
    let result = builtin_eval_buffer(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-eb-a").cloned(),
        Some(Value::fixnum(11))
    );
    assert_eq!(
        ev.obarray.symbol_value("lread-eb-b").cloned(),
        Some(Value::fixnum(12))
    );
}

#[test]
fn eval_buffer_accepts_shebang_reader_prefix() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("#!/usr/bin/env emacs --script\n(setq lread-eb-shebang 'ok)\n");
    }
    let result = builtin_eval_buffer(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-eb-shebang").cloned(),
        Some(Value::symbol("ok"))
    );
}

#[test]
fn eval_buffer_single_line_shebang_signals_end_of_file() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("#!/usr/bin/env emacs --script");
    }
    let result = builtin_eval_buffer(&mut ev, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file" && sig.data.is_empty()
    ));
}

#[test]
fn eval_buffer_preserves_utf8_bom_reader_error_shape() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("\u{feff}(setq lread-eb-bom 'ok)\n");
    }
    let result = builtin_eval_buffer(&mut ev, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "void-variable" && sig.data.len() == 1
    ));
}

#[test]
fn eval_buffer_uses_source_text_without_switching_current() {
    let mut ev = Context::new();
    let target = ev.buffers.create_buffer("*lread-eval-buffer-target*");
    {
        let target_buf = ev.buffers.get_mut(target).expect("target buffer");
        target_buf.insert("(setq lread-eb-current-name (buffer-name))");
    }
    let caller = ev.buffers.create_buffer("*lread-eval-buffer-caller*");
    ev.buffers.set_current(caller);

    let result = builtin_eval_buffer(&mut ev, vec![Value::make_buffer(target)]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-eb-current-name").cloned(),
        Some(Value::string("*lread-eval-buffer-caller*"))
    );
}

#[test]
fn eval_buffer_reports_designator_and_arity_errors() {
    let mut ev = Context::new();

    let missing = builtin_eval_buffer(&mut ev, vec![Value::string("*no-such-buffer*")]);
    assert!(matches!(
        missing,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error" && sig.data == vec![Value::string("No such buffer")]
    ));

    let bad_type = builtin_eval_buffer(&mut ev, vec![Value::fixnum(1)]);
    assert!(matches!(
        bad_type,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("stringp"), Value::fixnum(1)]
    ));

    let arity = builtin_eval_buffer(
        &mut ev,
        vec![
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(matches!(
        arity,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("eval-buffer"), Value::fixnum(6)]
    ));
}

#[test]
fn eval_region_evaluates_forms_in_range() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("(setq lread-er-a 1)\n(setq lread-er-b (+ lread-er-a 2))");
    }
    let end = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        Value::fixnum(buf.text.char_count() as i64 + 1)
    };

    let result = builtin_eval_region(&mut ev, vec![Value::fixnum(1), end]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-er-a").cloned(),
        Some(Value::fixnum(1))
    );
    assert_eq!(
        ev.obarray.symbol_value("lread-er-b").cloned(),
        Some(Value::fixnum(3))
    );
}

#[test]
fn eval_region_nil_or_reversed_bounds_are_noop() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("(setq lread-er-noop 9)");
    }
    ev.obarray.set_symbol_value("lread-er-noop", Value::fixnum(0));

    let nil_bounds = builtin_eval_region(&mut ev, vec![Value::NIL, Value::NIL]).unwrap();
    assert!(nil_bounds.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-er-noop").cloned(),
        Some(Value::fixnum(0))
    );

    let point_max = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        buf.text.char_count() as i64 + 1
    };
    let reversed =
        builtin_eval_region(&mut ev, vec![Value::fixnum(point_max), Value::fixnum(1)]).unwrap();
    assert!(reversed.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-er-noop").cloned(),
        Some(Value::fixnum(0))
    );
}

#[test]
fn eval_region_reports_type_range_and_arity_errors() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("(+ 1 2)");
    }
    let point_max = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        buf.text.char_count() as i64 + 1
    };

    let bad_start = builtin_eval_region(&mut ev, vec![Value::string("1"), Value::fixnum(point_max)]);
    assert!(matches!(
        bad_start,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data
                    == vec![Value::symbol("integer-or-marker-p"), Value::string("1")]
    ));

    let bad_end = builtin_eval_region(&mut ev, vec![Value::fixnum(1), Value::string("2")]);
    assert!(matches!(
        bad_end,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data
                    == vec![Value::symbol("integer-or-marker-p"), Value::string("2")]
    ));

    let range = builtin_eval_region(&mut ev, vec![Value::fixnum(1), Value::fixnum(999)]);
    assert!(matches!(
        range,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "args-out-of-range"
                && sig.data == vec![Value::fixnum(1), Value::fixnum(999)]
    ));

    let arity_low = builtin_eval_region(&mut ev, vec![]);
    assert!(matches!(
        arity_low,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("eval-region"), Value::fixnum(0)]
    ));

    let arity_high = builtin_eval_region(
        &mut ev,
        vec![
            Value::fixnum(1),
            Value::fixnum(point_max),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(matches!(
        arity_high,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("eval-region"), Value::fixnum(5)]
    ));
}

#[test]
fn eval_region_keeps_point_stable_without_side_effects() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("(setq lread-er-point 1)");
        buf.goto_char(0);
    }
    let end = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        Value::fixnum(buf.text.char_count() as i64 + 1)
    };
    let result = builtin_eval_region(&mut ev, vec![Value::fixnum(1), end]).unwrap();
    assert!(result.is_nil());
    let point = ev
        .buffers
        .current_buffer()
        .expect("current buffer")
        .point_char() as i64
        + 1;
    assert_eq!(point, 1);
}

#[test]
fn eval_region_accepts_shebang_reader_prefix() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("#!/usr/bin/env emacs --script\n(setq lread-er-shebang 'ok)\n");
    }
    let end = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        Value::fixnum(buf.text.char_count() as i64 + 1)
    };
    let result = builtin_eval_region(&mut ev, vec![Value::fixnum(1), end]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("lread-er-shebang").cloned(),
        Some(Value::symbol("ok"))
    );
}

#[test]
fn eval_region_single_line_shebang_signals_end_of_file() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("#!/usr/bin/env emacs --script");
    }
    let end = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        Value::fixnum(buf.text.char_count() as i64 + 1)
    };
    let result = builtin_eval_region(&mut ev, vec![Value::fixnum(1), end]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file" && sig.data.is_empty()
    ));
}

#[test]
fn eval_region_preserves_utf8_bom_reader_error_shape() {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("\u{feff}(setq lread-er-bom 'ok)\n");
    }
    let end = {
        let buf = ev.buffers.current_buffer().expect("current buffer");
        Value::fixnum(buf.text.char_count() as i64 + 1)
    };
    let result = builtin_eval_region(&mut ev, vec![Value::fixnum(1), end]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "void-variable" && sig.data.len() == 1
    ));
}

#[test]
fn read_event_returns_nil() {
    let mut ev = Context::new();
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn read_event_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_read_event(&mut ev, vec![Value::fixnum(123)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_event_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.recent_input_events(), &[Value::fixnum(97)]);
}

#[test]
fn read_event_sets_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let _ = builtin_read_event(&mut ev, vec![]).unwrap();
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_event_preserves_existing_command_keys_context() {
    let mut ev = Context::new();
    ev.set_read_command_keys(vec![Value::fixnum(97)]);
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::list(vec![Value::symbol("mouse-1")])]),
    );
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert!(result.is_cons());
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_event_with_seconds_does_not_set_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let _ = builtin_read_event(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(0)]).unwrap();
    assert_eq!(ev.read_command_keys(), &[]);
}

#[test]
fn read_event_with_positive_seconds_does_not_set_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let _ = builtin_read_event(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(1)]).unwrap();
    assert_eq!(ev.read_command_keys(), &[]);
}

#[test]
fn read_event_with_float_seconds_does_not_set_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let _ = builtin_read_event(
        &mut ev,
        vec![Value::NIL, Value::NIL, Value::make_float(0.25)],
    )
    .unwrap();
    assert_eq!(ev.read_command_keys(), &[]);
}

#[test]
fn read_event_with_interactive_timeout_returns_nil() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    let start = std::time::Instant::now();
    let result = builtin_read_event(
        &mut ev,
        vec![Value::NIL, Value::NIL, Value::make_float(0.01)],
    )
    .unwrap();
    drop(tx);

    assert!(result.is_nil());
    assert!(start.elapsed() < std::time::Duration::from_millis(250));
}

#[test]
fn read_event_with_non_nil_seconds_preserves_existing_command_keys_context() {
    let mut ev = Context::new();
    ev.set_read_command_keys(vec![Value::fixnum(97)]);
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(98)]));
    let _ = builtin_read_event(
        &mut ev,
        vec![Value::NIL, Value::NIL, Value::make_float(0.25)],
    )
    .unwrap();
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_event_with_nil_seconds_sets_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let _ = builtin_read_event(&mut ev, vec![Value::NIL, Value::NIL, Value::NIL]).unwrap();
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_event_consumes_non_character_event_and_preserves_tail() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::fixnum(97)]),
    );
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::symbol("foo"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::fixnum(97)]))
    );
}

#[test]
fn read_event_consumes_character_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::char('a')]));
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::NIL)
    );
}

#[test]
fn read_event_preserves_trailing_events_after_non_character() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::char('a')]),
    );
    let result = builtin_read_event(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::symbol("foo"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::char('a')]))
    );
}

#[test]
fn read_event_rejects_more_than_three_args() {
    let mut ev = Context::new();
    let result = builtin_read_event(
        &mut ev,
        vec![
            Value::string("key: "),
            Value::NIL,
            Value::fixnum(0),
            Value::NIL,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_char_exclusive_returns_nil() {
    let mut ev = Context::new();
    let result = builtin_read_char_exclusive(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn read_char_exclusive_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_read_char_exclusive(&mut ev, vec![Value::fixnum(123)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_char_exclusive_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let result = builtin_read_char_exclusive(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_char_exclusive_with_seconds_does_not_set_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let result =
        builtin_read_char_exclusive(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[]);
}

#[test]
fn read_char_exclusive_with_nil_seconds_sets_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(97)]));
    let result =
        builtin_read_char_exclusive(&mut ev, vec![Value::NIL, Value::NIL, Value::NIL]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_char_exclusive_preserves_existing_command_keys_context() {
    let mut ev = Context::new();
    ev.set_read_command_keys(vec![Value::fixnum(97)]);
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::fixnum(98)]));
    let result =
        builtin_read_char_exclusive(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_int(), Some(98));
    assert_eq!(ev.read_command_keys(), &[Value::fixnum(97)]);
}

#[test]
fn read_char_exclusive_rejects_more_than_three_args() {
    let mut ev = Context::new();
    let result = builtin_read_char_exclusive(
        &mut ev,
        vec![
            Value::string("key: "),
            Value::NIL,
            Value::fixnum(0),
            Value::NIL,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_char_exclusive_skips_non_character_events() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::fixnum(97)]),
    );
    let result = builtin_read_char_exclusive(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(
        ev.recent_input_events(),
        &[Value::symbol("foo"), Value::fixnum(97)]
    );
}

#[test]
fn read_char_exclusive_skips_non_character_and_empty_tail() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::fixnum(97)]),
    );
    let result =
        builtin_read_char_exclusive(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::NIL),
    );
}

#[test]
fn read_char_exclusive_skips_non_character_and_leaves_tail() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::fixnum(97), Value::fixnum(98)]),
    );
    let result =
        builtin_read_char_exclusive(&mut ev, vec![Value::NIL, Value::NIL, Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::fixnum(98)])),
    );
}

#[test]
fn get_load_suffixes_returns_list() {
    // The stateless variant is hardcoded to return (".el" "") matching
    // NeoVM's default load-suffixes and load-file-rep-suffixes.
    let result = builtin_get_load_suffixes(vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_str(), Some(".el"));
    assert_eq!(items[1].as_str(), Some(""));
}

#[test]
fn get_load_suffixes_rejects_over_arity() {
    let result = builtin_get_load_suffixes(vec![Value::NIL]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn locate_file_finds_first_matching_suffix() {
    let mut ctx = test_eval_ctx();
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-locate-file-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(dir.join("probe.el"), "(setq vm-locate 1)\n").expect("write .el");
    fs::write(dir.join("probe.elc"), "compiled").expect("write .elc");

    let result = builtin_locate_file(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(dir.to_string_lossy())]),
            Value::list(vec![Value::string(".el"), Value::string(".elc")]),
        ],
    )
    .expect("locate-file should succeed");
    let found = result.as_str().expect("locate-file should return path");
    assert!(
        found.ends_with("probe.el"),
        "expected first matching suffix (.el), got {found}",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn locate_file_respects_symbol_predicates() {
    let mut ctx = test_eval_ctx();
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-locate-file-predicate-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(dir.join("probe.el"), "(setq vm-locate 1)\n").expect("write .el");

    let regular = builtin_locate_file(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(dir.to_string_lossy())]),
            Value::list(vec![Value::string(".el")]),
            Value::symbol("file-regular-p"),
        ],
    )
    .expect("locate-file with file-regular-p should evaluate");
    assert!(
        regular.as_str().is_some(),
        "regular-file predicate should accept candidate",
    );

    let directory = builtin_locate_file(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(dir.to_string_lossy())]),
            Value::list(vec![Value::string(".el")]),
            Value::symbol("file-directory-p"),
        ],
    )
    .expect("locate-file with file-directory-p should evaluate");
    assert!(directory.is_nil(), "directory predicate should reject file");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn locate_file_unknown_predicate_defaults_to_truthy_match() {
    let mut ctx = test_eval_ctx();
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-locate-file-bad-predicate-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(dir.join("probe.el"), "(setq vm-locate 1)\n").expect("write .el");

    let result = builtin_locate_file(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(dir.to_string_lossy())]),
            Value::list(vec![Value::string(".el")]),
            Value::symbol("definitely-not-a-real-predicate"),
        ],
    )
    .expect("locate-file should evaluate");
    let found = result
        .as_str()
        .expect("unknown predicate should not prevent match");
    assert!(found.ends_with("probe.el"), "unexpected result: {found}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn locate_file_internal_returns_nil_when_missing() {
    let mut ctx = test_eval_ctx();
    let result = builtin_locate_file_internal(
        &mut ctx,
        vec![
            Value::string("definitely-missing-neovm-file"),
            Value::list(vec![Value::string(".")]),
            Value::list(vec![Value::string(".el")]),
        ],
    )
    .expect("locate-file-internal should evaluate");
    assert!(result.is_nil());
}

#[test]
fn locate_file_internal_finds_requested_suffix() {
    let mut ctx = test_eval_ctx();
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-locate-file-internal-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(dir.join("probe.elc"), "compiled").expect("write .elc");

    let result = builtin_locate_file_internal(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(dir.to_string_lossy())]),
            Value::list(vec![Value::string(".elc")]),
        ],
    )
    .expect("locate-file-internal should succeed");
    let found = result
        .as_str()
        .expect("locate-file-internal should return path");
    assert!(
        found.ends_with("probe.elc"),
        "expected .elc resolution, got {found}",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn locate_file_internal_treats_tilde_prefixed_names_as_absolute_like_gnu() {
    let mut ctx = test_eval_ctx();
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let home = std::env::var("HOME").expect("HOME must exist for locate-file tilde test");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::path::Path::new(&home).join(format!("neovm-locate-file-home-{unique}"));
    fs::create_dir_all(&dir).expect("create temp dir in HOME");

    let tilde_name = format!(
        "~/{}",
        dir.file_name()
            .expect("temp dir basename")
            .to_string_lossy()
    );

    let result = builtin_locate_file_internal(
        &mut ctx,
        vec![
            Value::string(&tilde_name),
            Value::list(vec![Value::string("./")]),
            Value::NIL,
            Value::symbol("file-directory-p"),
        ],
    )
    .expect("locate-file-internal tilde path should evaluate");

    assert_eq!(
        result.as_str(),
        Some(dir.to_string_lossy().as_ref()),
        "expected locate-file-internal to expand ~/ paths like GNU"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn locate_file_rejects_over_arity() {
    let mut ctx = test_eval_ctx();
    let result = builtin_locate_file(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(".")]),
            Value::list(vec![Value::string(".el")]),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn locate_file_internal_rejects_over_arity() {
    let mut ctx = test_eval_ctx();
    let result = builtin_locate_file_internal(
        &mut ctx,
        vec![
            Value::string("probe"),
            Value::list(vec![Value::string(".")]),
            Value::list(vec![Value::string(".el")]),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_coding_system_signals_batch_eof() {
    let result = builtin_read_coding_system(vec![Value::string("")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "end-of-file"
                && sig.data == vec![Value::string("Error reading from stdin")]
    ));
}

#[test]
fn read_coding_system_validates_prompt_type_and_arity() {
    let bad_prompt = builtin_read_coding_system(vec![Value::fixnum(1)]);
    assert!(matches!(
        bad_prompt,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("stringp"), Value::fixnum(1)]
    ));

    let arity = builtin_read_coding_system(vec![Value::string(""), Value::NIL, Value::NIL]);
    assert!(matches!(
        arity,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("read-coding-system"), Value::fixnum(3)]
    ));
}

#[test]
fn read_non_nil_coding_system_signals_batch_eof() {
    let result = builtin_read_non_nil_coding_system(vec![Value::string("")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "end-of-file"
                && sig.data == vec![Value::string("Error reading from stdin")]
    ));
}

#[test]
fn read_non_nil_coding_system_validates_prompt_type_and_arity() {
    let bad_prompt = builtin_read_non_nil_coding_system(vec![Value::fixnum(1)]);
    assert!(matches!(
        bad_prompt,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("stringp"), Value::fixnum(1)]
    ));

    let arity = builtin_read_non_nil_coding_system(vec![Value::string(""), Value::NIL]);
    assert!(matches!(
        arity,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data
                    == vec![Value::symbol("read-non-nil-coding-system"), Value::fixnum(2)]
    ));
}
