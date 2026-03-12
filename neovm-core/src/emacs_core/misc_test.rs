use super::super::intern::intern;
use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator};
use crate::emacs_core::string_escape;
use crate::emacs_core::{format_eval_result, parse_forms};

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

// ----- copy-alist -----

#[test]
fn copy_alist_basic() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::Int(1)),
        Value::cons(Value::symbol("b"), Value::Int(2)),
    ]);
    let result = builtin_copy_alist(vec![alist]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    // Original and copy should have equal structure
    assert!(equal_value(&alist, &result, 0));
    // But the cons cells should not be eq (different heap objects)
    if let (Value::Cons(a), Value::Cons(b)) = (&items[0], &list_to_vec(&alist).unwrap()[0]) {
        assert_ne!(a, b);
    }
}

#[test]
fn copy_alist_empty() {
    let result = builtin_copy_alist(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

// ----- rassoc / rassq -----

#[test]
fn rassoc_found() {
    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::Int(1)),
        Value::cons(Value::symbol("b"), Value::Int(2)),
        Value::cons(Value::symbol("c"), Value::Int(3)),
    ]);
    let result = builtin_rassoc(vec![Value::Int(2), alist]).unwrap();
    // Should return (b . 2)
    if let Value::Cons(cell) = &result {
        let pair = read_cons(*cell);
        assert!(eq_value(&pair.car, &Value::symbol("b")));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn rassoc_not_found() {
    let alist = Value::list(vec![Value::cons(Value::symbol("a"), Value::Int(1))]);
    let result = builtin_rassoc(vec![Value::Int(99), alist]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn rassq_found() {
    let alist = Value::list(vec![
        Value::cons(Value::symbol("x"), Value::symbol("yes")),
        Value::cons(Value::symbol("y"), Value::symbol("no")),
    ]);
    let result = builtin_rassq(vec![Value::symbol("yes"), alist]).unwrap();
    if let Value::Cons(cell) = &result {
        let pair = read_cons(*cell);
        assert!(eq_value(&pair.car, &Value::symbol("x")));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn rassq_not_found() {
    let alist = Value::list(vec![Value::cons(Value::symbol("a"), Value::Int(1))]);
    let result = builtin_rassq(vec![Value::Int(99), alist]).unwrap();
    assert!(result.is_nil());
}

// ----- assoc-default -----

#[test]
fn assoc_default_bootstrap_matches_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'assoc-default))
        (assoc-default "key" '(("key" . 42)))
        (assoc-default "missing" '(("key" . 42)) nil -1)
        (assoc-default 'foo '((foo . 10)) 'eq)
        (assoc-default 'foo '(foo) nil 'fallback)
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK 42");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK 10");
    assert_eq!(results[4], "OK fallback");
}

#[test]
fn assoc_default_bootstrap_error_shapes_match_gnu_subr() {
    let results = bootstrap_eval(
        r#"
        (condition-case err
            (assoc-default 'foo 1)
          (wrong-type-argument (list (car err) (nth 1 err))))
        (condition-case err
            (assoc-default 'foo '((foo . 10)) 1)
          (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK (wrong-type-argument listp)");
    assert_eq!(results[1], "OK invalid-function");
}

// ----- make-list -----

#[test]
fn make_list_basic() {
    let result = builtin_make_list(vec![Value::Int(3), Value::symbol("x")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 3);
    for item in &items {
        assert!(eq_value(item, &Value::symbol("x")));
    }
}

#[test]
fn make_list_zero() {
    let result = builtin_make_list(vec![Value::Int(0), Value::Int(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn make_list_validates_wholenump_length() {
    let negative = builtin_make_list(vec![Value::Int(-1), Value::Int(1)]).unwrap_err();
    let float =
        builtin_make_list(vec![Value::Float(3.2, next_float_id()), Value::Int(1)]).unwrap_err();
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("wholenump"), Value::Int(-1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match float {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("wholenump"),
                    Value::Float(3.2, next_float_id())
                ]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn string_repeat_basic() {
    let result = builtin_string_repeat(vec![Value::string("ab"), Value::Int(3)]).unwrap();
    assert_eq!(result.as_str().unwrap(), "ababab");
}

#[test]
fn string_repeat_zero() {
    let result = builtin_string_repeat(vec![Value::string("ab"), Value::Int(0)]).unwrap();
    assert_eq!(result.as_str().unwrap(), "");
}

#[test]
fn string_repeat_errors() {
    assert!(builtin_string_repeat(vec![]).is_err());
    assert!(builtin_string_repeat(vec![Value::string("ab")]).is_err());
    assert!(builtin_string_repeat(vec![Value::string("ab"), Value::Int(-1)]).is_err());
    assert!(builtin_string_repeat(vec![Value::Int(1), Value::Int(2)]).is_err());
}

// ----- safe-length -----

#[test]
fn safe_length_proper_list() {
    let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let result = builtin_safe_length(vec![list]).unwrap();
    assert!(eq_value(&result, &Value::Int(3)));
}

#[test]
fn safe_length_nil() {
    let result = builtin_safe_length(vec![Value::Nil]).unwrap();
    assert!(eq_value(&result, &Value::Int(0)));
}

#[test]
fn safe_length_non_list() {
    let result = builtin_safe_length(vec![Value::Int(42)]).unwrap();
    assert!(eq_value(&result, &Value::Int(0)));
}

// ----- subst-char-in-string -----

#[test]
fn subst_char_basic() {
    let result = builtin_subst_char_in_string(vec![
        Value::Char('.'),
        Value::Char('/'),
        Value::string("a.b.c"),
    ])
    .unwrap();
    assert_eq!(result.as_str().unwrap(), "a/b/c");
}

#[test]
fn subst_char_no_match() {
    let result = builtin_subst_char_in_string(vec![
        Value::Char('z'),
        Value::Char('!'),
        Value::string("hello"),
    ])
    .unwrap();
    assert_eq!(result.as_str().unwrap(), "hello");
}

// ----- string encoding identity stubs -----

#[test]
fn string_to_multibyte_identity() {
    let s = Value::string("hello");
    let result = builtin_string_to_multibyte(vec![s]).unwrap();
    assert!(equal_value(&s, &result, 0));
}

#[test]
fn string_to_multibyte_converts_unibyte_high_bytes_to_raw_byte_chars() {
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_to_multibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 2);
    assert_eq!(
        string_escape::decode_storage_char_codes(out),
        vec![0x3FFFFF]
    );
}

#[test]
fn string_to_unibyte_ascii_storage() {
    let result = builtin_string_to_unibyte(vec![Value::string("world")]).unwrap();
    let s = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(s), 5);
    assert_eq!(
        string_escape::decode_storage_char_codes(s),
        vec![119, 111, 114, 108, 100]
    );
}

#[test]
fn string_to_unibyte_rejects_unicode_scalar() {
    let result = builtin_string_to_unibyte(vec![Value::string("é")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Cannot convert character at index 0 to unibyte"
                )]
            );
        }
        other => panic!("expected conversion error, got {other:?}"),
    }
}

#[test]
fn string_to_unibyte_preserves_existing_unibyte_storage() {
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_to_unibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 1);
    assert_eq!(string_escape::decode_storage_char_codes(out), vec![255]);
}

#[test]
fn string_as_unibyte_utf8_bytes_for_unicode() {
    let result = builtin_string_as_unibyte(vec![Value::string("é")]).unwrap();
    let s = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(s), 2);
    assert_eq!(string_escape::decode_storage_char_codes(s), vec![195, 169]);
}

#[test]
fn string_as_unibyte_ascii_passthrough_bytes() {
    let result = builtin_string_as_unibyte(vec![Value::string("test")]).unwrap();
    let s = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(s), 4);
    assert_eq!(
        string_escape::decode_storage_char_codes(s),
        vec![116, 101, 115, 116]
    );
}

#[test]
fn string_as_unibyte_preserves_unibyte_storage_bytes() {
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_as_unibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 1);
    assert_eq!(string_escape::decode_storage_char_codes(out), vec![255]);
}

#[test]
fn string_as_multibyte_identity_for_multibyte_input() {
    let s = Value::string("test");
    let result = builtin_string_as_multibyte(vec![s]).unwrap();
    assert!(equal_value(&s, &result, 0));
}

#[test]
fn string_as_multibyte_converts_unibyte_high_bytes_to_raw_byte_chars() {
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_as_multibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 2);
    assert_eq!(
        string_escape::decode_storage_char_codes(out),
        vec![0x3FFFFF]
    );
}

// ----- char encoding conversions -----

#[test]
fn unibyte_char_to_multibyte_ascii_identity() {
    let result = builtin_unibyte_char_to_multibyte(vec![Value::Int(65)]).unwrap();
    assert!(eq_value(&result, &Value::Int(65)));
}

#[test]
fn unibyte_char_to_multibyte_high_byte_maps_to_raw_range() {
    let result = builtin_unibyte_char_to_multibyte(vec![Value::Int(255)]).unwrap();
    assert!(eq_value(&result, &Value::Int(0x3FFFFF)));
}

#[test]
fn unibyte_char_to_multibyte_rejects_non_unibyte_code() {
    let result = builtin_unibyte_char_to_multibyte(vec![Value::Int(256)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Not a unibyte character: 256")]
            );
        }
        other => panic!("expected conversion error, got {other:?}"),
    }
}

#[test]
fn multibyte_char_to_unibyte_ascii_passthrough() {
    let result = builtin_multibyte_char_to_unibyte(vec![Value::Int(65)]).unwrap();
    assert!(eq_value(&result, &Value::Int(65)));
}

#[test]
fn multibyte_char_to_unibyte_raw_range_maps_to_byte() {
    let result = builtin_multibyte_char_to_unibyte(vec![Value::Int(0x3FFFFF)]).unwrap();
    assert!(eq_value(&result, &Value::Int(255)));
}

#[test]
fn multibyte_char_to_unibyte_returns_minus_one_for_non_unibyte_unicode() {
    let result = builtin_multibyte_char_to_unibyte(vec![Value::Int(256)]).unwrap();
    assert!(eq_value(&result, &Value::Int(-1)));
}

// ----- locale-info -----

#[test]
fn locale_info_codeset_returns_utf8() {
    let result = builtin_locale_info(vec![Value::symbol("codeset")]).unwrap();
    assert_eq!(result.as_str(), Some("UTF-8"));
}

#[test]
fn locale_info_days_months_and_paper_return_oracle_shapes() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let days = builtin_locale_info(vec![Value::symbol("days")]).unwrap();
    let days_vec = match days {
        Value::Vector(v) => with_heap(|h| h.get_vector(v).clone()),
        other => panic!("days should be a vector, got {other:?}"),
    };
    assert_eq!(days_vec.len(), 7);
    assert_eq!(days_vec[0], Value::string("Sunday"));
    assert_eq!(days_vec[6], Value::string("Saturday"));

    let months = builtin_locale_info(vec![Value::symbol("months")]).unwrap();
    let months_vec = match months {
        Value::Vector(v) => with_heap(|h| h.get_vector(v).clone()),
        other => panic!("months should be a vector, got {other:?}"),
    };
    assert_eq!(months_vec.len(), 12);
    assert_eq!(months_vec[0], Value::string("January"));
    assert_eq!(months_vec[11], Value::string("December"));

    let paper = builtin_locale_info(vec![Value::symbol("paper")]).unwrap();
    assert_eq!(paper, Value::list(vec![Value::Int(210), Value::Int(297)]));
}

#[test]
fn locale_info_unknown_or_non_symbol_items_return_nil() {
    let result = builtin_locale_info(vec![Value::symbol("time")]).unwrap();
    assert!(result.is_nil());
    let result = builtin_locale_info(vec![Value::string("codeset")]).unwrap();
    assert!(result.is_nil());
    let result = builtin_locale_info(vec![Value::Int(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn display_line_numbers_update_width_is_noop() {
    let result = builtin_display_line_numbers_update_width(vec![]).unwrap();
    assert_eq!(result, Value::Nil);
}

#[test]
fn display_line_numbers_update_width_arity() {
    assert!(builtin_display_line_numbers_update_width(vec![Value::Nil]).is_err());
}

// ----- eval-dependent builtins (need Evaluator) -----

#[test]
fn recursion_depth_zero() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_recursion_depth(&mut eval, vec![]).unwrap();
    // At top level, depth is 0
    assert!(eq_value(&result, &Value::Int(0)));
}

#[test]
fn backtrace_frame_basic_shape() {
    let mut eval = super::super::eval::Evaluator::new();
    let frame0 = builtin_backtrace_frame(&mut eval, vec![Value::Int(0)]).unwrap();
    let items0 = list_to_vec(&frame0).expect("frame0 should be a list");
    assert_eq!(items0.first(), Some(&Value::True));
    assert_eq!(items0.get(1), Some(&Value::symbol("backtrace-frame")));

    let frame1 = builtin_backtrace_frame(&mut eval, vec![Value::Int(1)]).unwrap();
    let items1 = list_to_vec(&frame1).expect("frame1 should be a list");
    assert_eq!(items1.first(), Some(&Value::True));
    assert_eq!(items1.get(1), Some(&Value::symbol("eval")));

    let frame2 = builtin_backtrace_frame(&mut eval, vec![Value::Int(2)]).unwrap();
    assert!(frame2.is_list());
}

#[test]
fn backtrace_frame_handles_base_and_depth() {
    let mut eval = super::super::eval::Evaluator::new();

    let with_nil_base =
        builtin_backtrace_frame(&mut eval, vec![Value::Int(0), Value::Nil]).unwrap();
    assert!(with_nil_base.is_list());
    let items = list_to_vec(&with_nil_base).expect("list");
    assert_eq!(items.last(), Some(&Value::Nil));

    let with_truthy_base =
        builtin_backtrace_frame(&mut eval, vec![Value::Int(0), Value::True]).unwrap();
    assert!(with_truthy_base.is_nil());

    let deep = builtin_backtrace_frame(&mut eval, vec![Value::Int(50)]).unwrap();
    assert!(deep.is_nil());
}

#[test]
fn backtrace_frame_validation() {
    let mut eval = super::super::eval::Evaluator::new();

    let missing = builtin_backtrace_frame(&mut eval, vec![]);
    assert!(matches!(
        missing,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame"), Value::Int(0)]
    ));

    let over = builtin_backtrace_frame(&mut eval, vec![Value::Int(0), Value::Nil, Value::Nil]);
    assert!(matches!(
        over,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame"), Value::Int(3)]
    ));

    let bad_nil = builtin_backtrace_frame(&mut eval, vec![Value::Nil]);
    assert!(matches!(
        bad_nil,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::Nil]
    ));

    let bad_negative = builtin_backtrace_frame(&mut eval, vec![Value::Int(-1)]);
    assert!(matches!(
        bad_negative,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::Int(-1)]
    ));
}

#[test]
fn backtrace_helper_stubs_shape_and_errors() {
    let mut eval = super::super::eval::Evaluator::new();
    let thread = super::super::threads::builtin_current_thread(&mut eval, vec![]).unwrap();
    let frames = builtin_backtrace_frames_from_thread(&mut eval, vec![thread]).unwrap();
    assert!(frames.is_list());
    assert!(matches!(
        builtin_backtrace_frames_from_thread(&mut eval, vec![Value::Nil]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("threadp"), Value::Nil]
    ));

    assert!(matches!(
        builtin_backtrace_locals(&mut eval, vec![Value::Nil]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::Nil]
    ));
    assert!(matches!(
        builtin_backtrace_locals(&mut eval, vec![Value::Int(0)]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::Int(-1)]
    ));
    assert!(matches!(
        builtin_backtrace_eval(&mut eval, vec![Value::Int(0), Value::Nil]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::Nil]
    ));
    // backtrace-frame--internal now returns nil (stub) rather than
    // signaling invalid-function.
    let result =
        builtin_backtrace_frame_internal(&mut eval, vec![Value::Int(0), Value::Int(0), Value::Nil]);
    assert_eq!(result.unwrap(), Value::Nil);
}

#[test]
fn backtrace_helper_stubs_arity_checks() {
    let mut eval = super::super::eval::Evaluator::new();
    assert!(matches!(
        builtin_backtrace_debug(&mut eval, vec![]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-debug"), Value::Int(0)]
    ));
    assert!(matches!(
        builtin_backtrace_debug(&mut eval, vec![Value::Int(0)]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-debug"), Value::Int(1)]
    ));
    assert!(matches!(
        builtin_backtrace_frame_internal(&mut eval, vec![]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame--internal"), Value::Int(0)]
    ));
}

// ----- special form: save-current-buffer -----

#[test]
fn sf_save_current_buffer_restores() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    // Create a buffer and make it current
    let buf_id = ev.buffers.create_buffer("*test*");
    ev.buffers.set_current(buf_id);

    // save-current-buffer with body that just returns 42
    let tail = [Expr::Int(42)];
    let result = sf_save_current_buffer(&mut ev, &tail).unwrap();
    assert!(eq_value(&result, &Value::Int(42)));
    // Current buffer should still be *test*
    assert_eq!(ev.buffers.current_buffer().unwrap().id, buf_id);
}

// ----- special form: track-mouse -----

#[test]
fn sf_track_mouse_evaluates_body() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let tail = [Expr::Int(99)];
    let result = sf_track_mouse(&mut ev, &tail).unwrap();
    assert!(eq_value(&result, &Value::Int(99)));
}

// ----- special form: with-syntax-table -----

#[test]
fn sf_with_syntax_table_evaluates_body() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let tail = [
        Expr::List(vec![Expr::Symbol(intern("make-syntax-table"))]),
        Expr::Int(30),
    ];
    let result = sf_with_syntax_table(&mut ev, &tail).unwrap();
    assert!(eq_value(&result, &Value::Int(30)));
}

#[test]
fn sf_with_syntax_table_needs_args() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let tail: [Expr; 0] = [];
    let result = sf_with_syntax_table(&mut ev, &tail);
    assert!(result.is_err());
}

#[test]
fn sf_with_syntax_table_restores_original_table_on_success() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let original = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    let tail = [
        Expr::List(vec![Expr::Symbol(intern("make-syntax-table"))]),
        Expr::Int(1),
    ];
    sf_with_syntax_table(&mut ev, &tail).unwrap();
    let restored = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    assert!(eq_value(&restored, &original));
}

#[test]
fn sf_with_syntax_table_restores_original_table_on_error() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let original = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    let tail = [
        Expr::List(vec![Expr::Symbol(intern("make-syntax-table"))]),
        Expr::Symbol(intern("__missing_with_syntax_table_symbol__")),
    ];
    let _ = sf_with_syntax_table(&mut ev, &tail);
    let restored = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    assert!(eq_value(&restored, &original));
}

// ----- special form: with-temp-buffer -----

#[test]
fn sf_with_temp_buffer_returns_body_result() {
    use super::super::expr::Expr;
    let mut ev = super::super::eval::Evaluator::new();
    let tail = [Expr::Int(77)];
    let result = sf_with_temp_buffer(&mut ev, &tail).unwrap();
    assert!(eq_value(&result, &Value::Int(77)));
}
