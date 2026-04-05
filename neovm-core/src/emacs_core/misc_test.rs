use super::*;
use crate::emacs_core::string_escape;
use crate::emacs_core::{Context, format_eval_result};
use crate::test_utils::load_minimal_gnu_backquote_runtime;

fn bootstrap_eval(src: &str) -> Vec<String> {
    let mut ev = Context::new();
    load_minimal_gnu_backquote_runtime(&mut ev);
    ev.eval_str_each(src)
        .iter()
        .map(format_eval_result)
        .collect()
}

// ----- copy-alist -----

#[test]
fn copy_alist_basic() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::fixnum(1)),
        Value::cons(Value::symbol("b"), Value::fixnum(2)),
    ]);
    let result = builtin_copy_alist(vec![alist]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
    // Original and copy should have equal structure
    assert!(equal_value(&alist, &result, 0));
    // But the cons cells should not be eq (different heap objects)
    assert!(items[0].is_cons());
    let orig_first = &list_to_vec(&alist).unwrap()[0];
    assert!(orig_first.is_cons());
    assert_ne!(items[0], *orig_first);
}

#[test]
fn copy_alist_empty() {
    crate::test_utils::init_test_tracing();
    let result = builtin_copy_alist(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

// ----- rassoc / rassq -----

#[test]
fn rassoc_found() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::fixnum(1)),
        Value::cons(Value::symbol("b"), Value::fixnum(2)),
        Value::cons(Value::symbol("c"), Value::fixnum(3)),
    ]);
    let result = builtin_rassoc(vec![Value::fixnum(2), alist]).unwrap();
    // Should return (b . 2)
    if result.is_cons() {
        let pair_car = result.cons_car();
        let pair_cdr = result.cons_cdr();
        assert!(eq_value(&pair_car, &Value::symbol("b")));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn rassoc_not_found() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![Value::cons(Value::symbol("a"), Value::fixnum(1))]);
    let result = builtin_rassoc(vec![Value::fixnum(99), alist]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn rassq_found() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![
        Value::cons(Value::symbol("x"), Value::symbol("yes")),
        Value::cons(Value::symbol("y"), Value::symbol("no")),
    ]);
    let result = builtin_rassq(vec![Value::symbol("yes"), alist]).unwrap();
    if result.is_cons() {
        let pair_car = result.cons_car();
        let pair_cdr = result.cons_cdr();
        assert!(eq_value(&pair_car, &Value::symbol("x")));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn rassq_not_found() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![Value::cons(Value::symbol("a"), Value::fixnum(1))]);
    let result = builtin_rassq(vec![Value::fixnum(99), alist]).unwrap();
    assert!(result.is_nil());
}

// ----- assoc-default -----

#[test]
fn assoc_default_bootstrap_matches_gnu_subr() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let result = builtin_make_list(vec![Value::fixnum(3), Value::symbol("x")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 3);
    for item in &items {
        assert!(eq_value(item, &Value::symbol("x")));
    }
}

#[test]
fn make_list_zero() {
    crate::test_utils::init_test_tracing();
    let result = builtin_make_list(vec![Value::fixnum(0), Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn make_list_validates_wholenump_length() {
    crate::test_utils::init_test_tracing();
    let negative = builtin_make_list(vec![Value::fixnum(-1), Value::fixnum(1)]).unwrap_err();
    let float = builtin_make_list(vec![Value::make_float(3.2), Value::fixnum(1)]).unwrap_err();
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("wholenump"), Value::fixnum(-1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match float {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("wholenump"), Value::make_float(3.2)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn string_repeat_basic() {
    crate::test_utils::init_test_tracing();
    let result = builtin_string_repeat(vec![Value::string("ab"), Value::fixnum(3)]).unwrap();
    assert_eq!(result.as_str().unwrap(), "ababab");
}

#[test]
fn string_repeat_zero() {
    crate::test_utils::init_test_tracing();
    let result = builtin_string_repeat(vec![Value::string("ab"), Value::fixnum(0)]).unwrap();
    assert_eq!(result.as_str().unwrap(), "");
}

#[test]
fn string_repeat_errors() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_string_repeat(vec![]).is_err());
    assert!(builtin_string_repeat(vec![Value::string("ab")]).is_err());
    assert!(builtin_string_repeat(vec![Value::string("ab"), Value::fixnum(-1)]).is_err());
    assert!(builtin_string_repeat(vec![Value::fixnum(1), Value::fixnum(2)]).is_err());
}

// ----- safe-length -----

#[test]
fn safe_length_proper_list() {
    crate::test_utils::init_test_tracing();
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    let result = builtin_safe_length(vec![list]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(3)));
}

#[test]
fn safe_length_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_safe_length(vec![Value::NIL]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(0)));
}

#[test]
fn safe_length_non_list() {
    crate::test_utils::init_test_tracing();
    let result = builtin_safe_length(vec![Value::fixnum(42)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(0)));
}

// ----- subst-char-in-string -----

#[test]
fn subst_char_basic() {
    crate::test_utils::init_test_tracing();
    let result = builtin_subst_char_in_string(vec![
        Value::char('.'),
        Value::char('/'),
        Value::string("a.b.c"),
    ])
    .unwrap();
    assert_eq!(result.as_str().unwrap(), "a/b/c");
}

#[test]
fn subst_char_no_match() {
    crate::test_utils::init_test_tracing();
    let result = builtin_subst_char_in_string(vec![
        Value::char('z'),
        Value::char('!'),
        Value::string("hello"),
    ])
    .unwrap();
    assert_eq!(result.as_str().unwrap(), "hello");
}

// ----- string encoding identity stubs -----

#[test]
fn string_to_multibyte_identity() {
    crate::test_utils::init_test_tracing();
    let s = Value::string("hello");
    let result = builtin_string_to_multibyte(vec![s]).unwrap();
    assert!(equal_value(&s, &result, 0));
}

#[test]
fn string_to_multibyte_converts_unibyte_high_bytes_to_raw_byte_chars() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_to_unibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 1);
    assert_eq!(string_escape::decode_storage_char_codes(out), vec![255]);
}

#[test]
fn string_as_unibyte_utf8_bytes_for_unicode() {
    crate::test_utils::init_test_tracing();
    let result = builtin_string_as_unibyte(vec![Value::string("é")]).unwrap();
    let s = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(s), 2);
    assert_eq!(string_escape::decode_storage_char_codes(s), vec![195, 169]);
}

#[test]
fn string_as_unibyte_ascii_passthrough_bytes() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut s = String::new();
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let result = builtin_string_as_unibyte(vec![Value::string(s)]).unwrap();
    let out = result.as_str().unwrap();
    assert_eq!(string_escape::storage_byte_len(out), 1);
    assert_eq!(string_escape::decode_storage_char_codes(out), vec![255]);
}

#[test]
fn string_as_multibyte_identity_for_multibyte_input() {
    crate::test_utils::init_test_tracing();
    let s = Value::string("test");
    let result = builtin_string_as_multibyte(vec![s]).unwrap();
    assert!(equal_value(&s, &result, 0));
}

#[test]
fn string_as_multibyte_converts_unibyte_high_bytes_to_raw_byte_chars() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let result = builtin_unibyte_char_to_multibyte(vec![Value::fixnum(65)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(65)));
}

#[test]
fn unibyte_char_to_multibyte_high_byte_maps_to_raw_range() {
    crate::test_utils::init_test_tracing();
    let result = builtin_unibyte_char_to_multibyte(vec![Value::fixnum(255)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(0x3FFFFF)));
}

#[test]
fn unibyte_char_to_multibyte_rejects_non_unibyte_code() {
    crate::test_utils::init_test_tracing();
    let result = builtin_unibyte_char_to_multibyte(vec![Value::fixnum(256)]);
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
    crate::test_utils::init_test_tracing();
    let result = builtin_multibyte_char_to_unibyte(vec![Value::fixnum(65)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(65)));
}

#[test]
fn multibyte_char_to_unibyte_raw_range_maps_to_byte() {
    crate::test_utils::init_test_tracing();
    let result = builtin_multibyte_char_to_unibyte(vec![Value::fixnum(0x3FFFFF)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(255)));
}

#[test]
fn multibyte_char_to_unibyte_returns_minus_one_for_non_unibyte_unicode() {
    crate::test_utils::init_test_tracing();
    let result = builtin_multibyte_char_to_unibyte(vec![Value::fixnum(256)]).unwrap();
    assert!(eq_value(&result, &Value::fixnum(-1)));
}

// ----- locale-info -----

#[test]
fn locale_info_codeset_returns_utf8() {
    crate::test_utils::init_test_tracing();
    let result = builtin_locale_info(vec![Value::symbol("codeset")]).unwrap();
    assert_eq!(result.as_str(), Some("UTF-8"));
}

#[test]
fn locale_info_days_months_and_paper_return_oracle_shapes() {
    crate::test_utils::init_test_tracing();
    let days = builtin_locale_info(vec![Value::symbol("days")]).unwrap();
    let days_vec = match days.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => days.as_vector_data().unwrap().clone(),
        other => panic!("days should be a vector, got {other:?}"),
    };
    assert_eq!(days_vec.len(), 7);
    assert_eq!(days_vec[0], Value::string("Sunday"));
    assert_eq!(days_vec[6], Value::string("Saturday"));

    let months = builtin_locale_info(vec![Value::symbol("months")]).unwrap();
    let months_vec = match months.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => months.as_vector_data().unwrap().clone(),
        other => panic!("months should be a vector, got {other:?}"),
    };
    assert_eq!(months_vec.len(), 12);
    assert_eq!(months_vec[0], Value::string("January"));
    assert_eq!(months_vec[11], Value::string("December"));

    let paper = builtin_locale_info(vec![Value::symbol("paper")]).unwrap();
    assert_eq!(
        paper,
        Value::list(vec![Value::fixnum(210), Value::fixnum(297)])
    );
}

#[test]
fn locale_info_unknown_or_non_symbol_items_return_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_locale_info(vec![Value::symbol("time")]).unwrap();
    assert!(result.is_nil());
    let result = builtin_locale_info(vec![Value::string("codeset")]).unwrap();
    assert!(result.is_nil());
    let result = builtin_locale_info(vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn display_line_numbers_update_width_is_noop() {
    crate::test_utils::init_test_tracing();
    let result = builtin_display_line_numbers_update_width(vec![]).unwrap();
    assert_eq!(result, Value::NIL);
}

#[test]
fn display_line_numbers_update_width_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_display_line_numbers_update_width(vec![Value::NIL]).is_err());
}

// ----- eval-dependent builtins (need Context) -----

#[test]
fn recursion_depth_zero() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let result = builtin_recursion_depth(&mut eval, vec![]).unwrap();
    // At top level, depth is 0
    assert!(eq_value(&result, &Value::fixnum(0)));
}

#[test]
fn backtrace_frame_basic_shape() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let frame0 = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(0)]).unwrap();
    let items0 = list_to_vec(&frame0).expect("frame0 should be a list");
    assert_eq!(items0.first(), Some(&Value::T));
    assert_eq!(items0.get(1), Some(&Value::symbol("backtrace-frame")));

    let frame1 = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(1)]).unwrap();
    let items1 = list_to_vec(&frame1).expect("frame1 should be a list");
    assert_eq!(items1.first(), Some(&Value::T));
    assert_eq!(items1.get(1), Some(&Value::symbol("eval")));

    let frame2 = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(2)]).unwrap();
    assert!(frame2.is_list());
}

#[test]
fn backtrace_frame_handles_base_and_depth() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();

    let with_nil_base =
        builtin_backtrace_frame(&mut eval, vec![Value::fixnum(0), Value::NIL]).unwrap();
    assert!(with_nil_base.is_list());
    let items = list_to_vec(&with_nil_base).expect("list");
    assert_eq!(items.last(), Some(&Value::NIL));

    let with_truthy_base =
        builtin_backtrace_frame(&mut eval, vec![Value::fixnum(0), Value::T]).unwrap();
    assert!(with_truthy_base.is_nil());

    let deep = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(50)]).unwrap();
    assert!(deep.is_nil());
}

#[test]
fn backtrace_frame_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();

    let missing = builtin_backtrace_frame(&mut eval, vec![]);
    assert!(matches!(
        missing,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame"), Value::fixnum(0)]
    ));

    let over = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(0), Value::NIL, Value::NIL]);
    assert!(matches!(
        over,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame"), Value::fixnum(3)]
    ));

    let bad_nil = builtin_backtrace_frame(&mut eval, vec![Value::NIL]);
    assert!(matches!(
        bad_nil,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::NIL]
    ));

    let bad_negative = builtin_backtrace_frame(&mut eval, vec![Value::fixnum(-1)]);
    assert!(matches!(
        bad_negative,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::fixnum(-1)]
    ));
}

#[test]
fn backtrace_helper_stubs_shape_and_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let thread = super::super::threads::builtin_current_thread(&mut eval, vec![]).unwrap();
    let frames = builtin_backtrace_frames_from_thread(&mut eval, vec![thread]).unwrap();
    assert!(frames.is_list());
    assert!(matches!(
        builtin_backtrace_frames_from_thread(&mut eval, vec![Value::NIL]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("threadp"), Value::NIL]
    ));

    assert!(matches!(
        builtin_backtrace_locals(&mut eval, vec![Value::NIL]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::NIL]
    ));
    assert!(matches!(
        builtin_backtrace_locals(&mut eval, vec![Value::fixnum(0)]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::fixnum(-1)]
    ));
    assert!(matches!(
        builtin_backtrace_eval(&mut eval, vec![Value::fixnum(0), Value::NIL]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("wholenump"), Value::NIL]
    ));
}

#[test]
fn backtrace_helper_stubs_arity_checks() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    assert!(matches!(
        builtin_backtrace_debug(&mut eval, vec![]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-debug"), Value::fixnum(0)]
    ));
    assert!(matches!(
        builtin_backtrace_debug(&mut eval, vec![Value::fixnum(0)]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-debug"), Value::fixnum(1)]
    ));
    assert!(matches!(
        builtin_backtrace_frame_internal(&mut eval, vec![]),
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data == vec![Value::symbol("backtrace-frame--internal"), Value::fixnum(0)]
    ));
}

#[test]
fn backtrace_frame_internal_tracks_runtime_funcall_interactively_marker() {
    crate::test_utils::init_test_tracing();
    let mut ev = super::super::eval::Context::new();
    let results = ev.eval_str_each(
        r#"
        (progn
          (fset 'neovm--misc-bt-target
                (lambda ()
                  (interactive)
                  (let (frame)
                    (backtrace-frame--internal
                     (lambda (evald func args flags)
                       (setq frame (list evald func args flags)))
                     1
                     'neovm--misc-bt-target)
                    (nth 1 frame))))
          (unwind-protect
              (list
               (funcall-interactively 'neovm--misc-bt-target)
               (call-interactively 'neovm--misc-bt-target))
            (fmakunbound 'neovm--misc-bt-target)))
        "#,
    );
    assert_eq!(
        results.iter().map(format_eval_result).collect::<Vec<_>>(),
        vec!["OK (funcall-interactively funcall-interactively)"]
    );
}

// ----- special form: save-current-buffer -----

#[test]
fn sf_save_current_buffer_restores() {
    crate::test_utils::init_test_tracing();
    use super::super::expr::Expr;
    use crate::emacs_core::value::{ValueKind, VecLikeType};
    let mut ev = super::super::eval::Context::new();
    // Create a buffer and make it current
    let buf_id = ev.buffers.create_buffer("*test*");
    ev.buffers.set_current(buf_id);

    // save-current-buffer with body that just returns 42
    let tail = [Expr::Int(42)];
    let result = sf_save_current_buffer(&mut ev, &tail).unwrap();
    assert!(eq_value(&result, &Value::fixnum(42)));
    // Current buffer should still be *test*
    assert_eq!(ev.buffers.current_buffer().unwrap().id, buf_id);
}

// ----- with-syntax-table (Elisp macro in GNU subr.el:6394) -----

#[test]
fn sf_with_syntax_table_evaluates_body() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    load_minimal_gnu_backquote_runtime(&mut ev);
    let result = ev.eval_str("(with-syntax-table (make-syntax-table) 30)").expect("eval");
    assert!(eq_value(&result, &Value::fixnum(30)));
}

#[test]
fn sf_with_syntax_table_restores_original_table_on_success() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    load_minimal_gnu_backquote_runtime(&mut ev);
    let original = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    ev.eval_str("(with-syntax-table (make-syntax-table) 1)").expect("eval");
    let restored = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    assert!(eq_value(&restored, &original));
}

#[test]
fn with_syntax_table_restores_original_table_on_error() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    load_minimal_gnu_backquote_runtime(&mut ev);
    let original = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    let _ = ev.eval_str("(ignore-errors (with-syntax-table (make-syntax-table) missing-var))");
    let restored = crate::emacs_core::syntax::builtin_syntax_table(&mut ev, vec![]).unwrap();
    assert!(eq_value(&restored, &original));
}
