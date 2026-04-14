use crate::emacs_core::error::Flow;
use crate::emacs_core::value::{HashTableTest, Value, next_float_id};

#[test]
fn fillarray_vector_is_in_place() {
    crate::test_utils::init_test_tracing();
    let vec = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    let out = crate::emacs_core::builtins::builtin_fillarray(vec![vec, Value::fixnum(9)]).unwrap();
    assert_eq!(out, vec);
    if !out.is_vector() {
        panic!("expected vector");
    };
    let values = out.as_vector_data().unwrap().clone();
    assert_eq!(&*values, &[Value::fixnum(9), Value::fixnum(9)]);
}

#[test]
fn fillarray_bool_vector_preserves_layout_and_sets_bits() {
    crate::test_utils::init_test_tracing();
    let bv =
        crate::emacs_core::chartable::builtin_make_bool_vector(vec![Value::fixnum(4), Value::NIL])
            .unwrap();
    let out =
        crate::emacs_core::builtins::builtin_fillarray(vec![bv, Value::symbol("non-nil")]).unwrap();
    assert_eq!(out, bv);
    assert_eq!(
        crate::emacs_core::chartable::builtin_bool_vector_p(vec![bv]).unwrap(),
        Value::T
    );
    assert_eq!(
        crate::emacs_core::chartable::builtin_bool_vector_count_population(vec![bv]).unwrap(),
        Value::fixnum(4)
    );

    crate::emacs_core::builtins::builtin_fillarray(vec![bv, Value::NIL]).unwrap();
    assert_eq!(
        crate::emacs_core::chartable::builtin_bool_vector_count_population(vec![bv]).unwrap(),
        Value::fixnum(0)
    );
}

#[test]
fn fillarray_char_table_preserves_shape_and_updates_default_slot() {
    crate::test_utils::init_test_tracing();
    let table = crate::emacs_core::chartable::make_char_table_value(
        Value::symbol("syntax-table"),
        Value::fixnum(0),
    );
    crate::emacs_core::chartable::builtin_set_char_table_range(vec![
        table,
        Value::fixnum('a' as i64),
        Value::fixnum(9),
    ])
    .unwrap();

    let out =
        crate::emacs_core::builtins::builtin_fillarray(vec![table, Value::fixnum(7)]).unwrap();
    assert_eq!(out, table);
    assert_eq!(
        crate::emacs_core::chartable::builtin_char_table_p(vec![table]).unwrap(),
        Value::T
    );
    assert_eq!(
        crate::emacs_core::chartable::builtin_char_table_subtype(vec![table]).unwrap(),
        Value::symbol("syntax-table")
    );
    assert_eq!(
        crate::emacs_core::chartable::builtin_char_table_range(vec![
            table,
            Value::fixnum('a' as i64)
        ])
        .unwrap(),
        Value::fixnum(9)
    );
    assert_eq!(
        crate::emacs_core::chartable::builtin_char_table_range(vec![table, Value::NIL]).unwrap(),
        Value::fixnum(7)
    );
}

#[test]
fn external_debugging_rejects_negative_fixnum() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_external_debugging_output(vec![Value::fixnum(-1)])
            .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn define_hash_table_test_requires_symbol_name() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_define_hash_table_test(vec![
        Value::fixnum(1),
        Value::symbol("eq"),
        Value::symbol("sxhash-eq"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn face_attributes_as_vector_shape() {
    crate::test_utils::init_test_tracing();
    let out =
        crate::emacs_core::builtins::builtin_face_attributes_as_vector(vec![Value::NIL]).unwrap();
    if !out.is_vector() {
        panic!("expected vector");
    };
    let values = out.as_vector_data().unwrap().clone();
    assert_eq!(values.len(), 20);
}

#[test]
fn frame_face_hash_table_uses_eq_test() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let out = crate::emacs_core::xfaces::builtin_frame_face_hash_table(&mut eval, vec![]).unwrap();
    if !out.is_hash_table() {
        panic!("expected hash table");
    };
    assert!(matches!(
        out.as_hash_table().unwrap().test.clone(),
        HashTableTest::Eq
    ));
}

#[test]
fn font_match_p_requires_font_spec_values() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_font_match_p(vec![Value::NIL, Value::NIL])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn frame_set_was_invisible_returns_new_state() {
    crate::test_utils::init_test_tracing();
    let out =
        crate::emacs_core::builtins::builtin_frame_set_was_invisible(vec![Value::NIL, Value::T])
            .unwrap();
    assert_eq!(out, Value::T);
}

#[test]
fn frame_bottom_divider_width_rejects_non_frame_designator() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_frame_bottom_divider_width(vec![Value::fixnum(0)])
            .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn frame_scale_factor_defaults_to_one_float() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_frame_scale_factor(vec![]).unwrap();
    assert_eq!(out, Value::make_float(1.0));
}

#[test]
fn garbage_collect_maybe_requires_whole_number() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_garbage_collect_maybe(vec![Value::T]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gnutls_error_string_zero_is_success() {
    crate::test_utils::init_test_tracing();
    let out =
        crate::emacs_core::builtins::builtin_gnutls_error_string(vec![Value::fixnum(0)]).unwrap();
    assert_eq!(out, Value::string("Success."));
}

#[test]
fn gnutls_peer_status_warning_describe_rejects_non_symbol() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_gnutls_peer_status_warning_describe(vec![
        Value::fixnum(0),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gpm_mouse_start_signals_console_only_error() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_gpm_mouse_start(vec![]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn sqlite_version_returns_string() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_sqlite_version(vec![]).unwrap();
    assert!(out.is_string());
}

#[test]
fn inotify_valid_p_returns_nil() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_inotify_valid_p(vec![Value::fixnum(0)]).unwrap();
    assert_eq!(out, Value::NIL);
}

#[test]
fn sqlite_open_and_close_round_trip() {
    crate::test_utils::init_test_tracing();
    let db = crate::emacs_core::builtins::builtin_sqlite_open(vec![]).unwrap();
    let sqlitep = crate::emacs_core::builtins::builtin_sqlitep(vec![db]).unwrap();
    assert_eq!(sqlitep, Value::T);
    let closed = crate::emacs_core::builtins::builtin_sqlite_close(vec![db]).unwrap();
    assert_eq!(closed, Value::T);
}

#[test]
fn sqlite_execute_rejects_non_handle() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_sqlite_execute(vec![
        Value::NIL,
        Value::string("select 1"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn inotify_watch_lifecycle() {
    crate::test_utils::init_test_tracing();
    let watch = crate::emacs_core::builtins::builtin_inotify_add_watch(vec![
        Value::string("/tmp"),
        Value::NIL,
        Value::symbol("ignore"),
    ])
    .unwrap();
    let active = crate::emacs_core::builtins::builtin_inotify_valid_p(vec![watch]).unwrap();
    assert_eq!(active, Value::T);
    let removed = crate::emacs_core::builtins::builtin_inotify_rm_watch(vec![watch]).unwrap();
    assert_eq!(removed, Value::T);
    let inactive = crate::emacs_core::builtins::builtin_inotify_valid_p(vec![watch]).unwrap();
    assert_eq!(inactive, Value::NIL);
}

#[test]
fn inotify_rm_watch_invalid_descriptor_signals() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_inotify_rm_watch(vec![Value::fixnum(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-notify-error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gnutls_bye_requires_process() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_gnutls_bye(vec![Value::NIL, Value::NIL]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gnutls_format_certificate_requires_string() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_gnutls_format_certificate(vec![Value::NIL])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gnutls_hash_digest_nil_method_signals_error() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_gnutls_hash_digest(vec![
        Value::NIL,
        Value::string("a"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn gnutls_hash_mac_symbol_method_returns_string() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_gnutls_hash_mac(vec![
        Value::symbol("SHA256"),
        Value::string("k"),
        Value::string("a"),
    ])
    .unwrap();
    assert!(out.is_string());
}

#[test]
fn gnutls_symmetric_encrypt_accepts_optional_aad_slot() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_gnutls_symmetric_encrypt(vec![
        Value::symbol("AES-128-GCM"),
        Value::string("k"),
        Value::string("iv"),
        Value::string("data"),
        Value::string("aad"),
    ])
    .unwrap();
    assert_eq!(out, Value::NIL);
}

#[test]
fn handle_switch_frame_accepts_switch_frame_event_and_rejects_nil() {
    crate::test_utils::init_test_tracing();
    let frame_event = Value::list(vec![Value::symbol("switch-frame"), Value::make_frame(1)]);
    let out = crate::emacs_core::builtins::builtin_handle_switch_frame(vec![frame_event])
        .expect("switch-frame event should be accepted");
    assert_eq!(out, Value::NIL);

    let err =
        crate::emacs_core::builtins::builtin_handle_switch_frame(vec![Value::NIL]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn interactive_form_for_ignore_returns_interactive_list() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let out = crate::emacs_core::builtins::symbols::builtin_interactive_form(
        &mut eval,
        vec![Value::symbol("ignore")],
    )
    .unwrap();
    assert_eq!(
        out,
        Value::list(vec![Value::symbol("interactive"), Value::NIL])
    );
}

#[test]
fn lock_file_requires_string_argument() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_lock_file(vec![Value::NIL]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn unlock_file_requires_string_argument() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_unlock_file(vec![Value::NIL]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn inotify_add_watch_requires_string_path_argument() {
    crate::test_utils::init_test_tracing();
    let err = crate::emacs_core::builtins::builtin_inotify_add_watch(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn window_bottom_divider_width_rejects_non_window_designator() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_window_bottom_divider_width(vec![Value::fixnum(1)])
            .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("window-live-p")));
        }
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn treesit_available_p_reports_runtime_support() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_treesit_available_p(vec![]).unwrap();
    assert_eq!(out, Value::T);
}

#[test]
fn treesit_query_compile_validates_arity() {
    crate::test_utils::init_test_tracing();
    let err =
        crate::emacs_core::builtins::builtin_treesit_query_compile(vec![Value::NIL]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn internal_stack_stats_returns_nil() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_internal_stack_stats(vec![]).unwrap();
    assert_eq!(out, Value::NIL);
}

#[test]
fn internal_labeled_narrow_to_region_validates_arity() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let err = crate::emacs_core::builtins::builtin_internal_labeled_narrow_to_region(
        &mut eval,
        vec![Value::NIL, Value::NIL],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn lossage_size_defaults_to_three_hundred() {
    crate::test_utils::init_test_tracing();
    let out = crate::emacs_core::builtins::builtin_lossage_size(vec![]).unwrap();
    assert_eq!(out, Value::fixnum(300));
}
