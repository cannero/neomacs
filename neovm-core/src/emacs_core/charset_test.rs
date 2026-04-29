use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::pdump::dump_to_file;
use crate::emacs_core::value::ValueKind;

// -----------------------------------------------------------------------
// CharsetRegistry unit tests
// -----------------------------------------------------------------------

#[test]
fn registry_has_standard_charsets() {
    crate::test_utils::init_test_tracing();
    let reg = CharsetRegistry::new();
    assert!(reg.contains("ascii"));
    assert!(reg.contains("unicode"));
    assert!(reg.contains("unicode-bmp"));
    assert!(reg.contains("latin-iso8859-1"));
    assert!(reg.contains("emacs"));
    assert!(reg.contains("eight-bit"));
    assert!(!reg.contains("nonexistent"));
}

#[test]
fn registry_names_returns_all() {
    crate::test_utils::init_test_tracing();
    let reg = CharsetRegistry::new();
    let names = reg.names();
    assert_eq!(names.len(), 8);
    assert!(names.contains(&"ascii".to_string()));
    assert!(names.contains(&"unicode".to_string()));
}

#[test]
fn registry_priority_list() {
    crate::test_utils::init_test_tracing();
    let reg = CharsetRegistry::new();
    let prio = reg.priority_list();
    assert!(!prio.is_empty());
    // unicode should be the highest priority.
    assert_eq!(resolve_sym(prio[0]), "unicode");
}

#[test]
fn registry_plist_returns_empty_for_standard() {
    crate::test_utils::init_test_tracing();
    let reg = CharsetRegistry::new();
    let plist = reg.plist(intern("ascii")).unwrap();
    assert!(plist.is_empty());
}

#[test]
fn registry_plist_none_for_unknown() {
    crate::test_utils::init_test_tracing();
    let reg = CharsetRegistry::new();
    assert!(reg.plist(intern("nonexistent")).is_none());
}

// -----------------------------------------------------------------------
// Builtin tests: charsetp
// -----------------------------------------------------------------------

#[test]
fn charsetp_known() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charsetp(vec![Value::symbol("ascii")]).unwrap();
    assert!(r.is_t());
}

#[test]
fn charsetp_unknown() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charsetp(vec![Value::symbol("nonexistent")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_string_arg() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charsetp(vec![Value::string("unicode")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_non_symbol() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charsetp(vec![Value::fixnum(42)]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_charsetp(vec![]).is_err());
    assert!(builtin_charsetp(vec![Value::NIL, Value::NIL]).is_err());
}

#[test]
fn charset_list_returns_priority_order() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_list(vec![]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(items[0].is_symbol_named("unicode"));
    assert!(items.len() >= 2);
}

#[test]
fn unibyte_charset_returns_eight_bit() {
    crate::test_utils::init_test_tracing();
    let r = builtin_unibyte_charset(vec![]).unwrap();
    assert!(r.is_symbol_named("eight-bit"));
}

// -----------------------------------------------------------------------
// Builtin tests: charset-priority-list
// -----------------------------------------------------------------------

#[test]
fn charset_priority_list_full() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_priority_list(vec![]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(!items.is_empty());
    // First should be unicode.
    assert!(items[0].is_symbol_named("unicode"));
}

#[test]
fn charset_priority_list_highestp() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_priority_list(vec![Value::T]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("unicode"));
}

#[test]
fn charset_priority_list_highestp_nil() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_priority_list(vec![Value::NIL]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(items.len() > 1);
}

// -----------------------------------------------------------------------
// Builtin tests: set-charset-priority
// -----------------------------------------------------------------------

#[test]
fn registry_set_priority_reorders_and_dedups() {
    crate::test_utils::init_test_tracing();
    let mut reg = CharsetRegistry::new();
    reg.set_priority(&[intern("ascii"), intern("unicode"), intern("ascii")]);
    assert_eq!(resolve_sym(reg.priority[0]), "ascii");
    assert_eq!(resolve_sym(reg.priority[1]), "unicode");
}

#[test]
fn set_charset_priority_requires_at_least_one_arg() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_set_charset_priority(vec![]).is_err());
}

#[test]
fn set_charset_priority_rejects_unknown_charset() {
    crate::test_utils::init_test_tracing();
    let r = builtin_set_charset_priority(vec![Value::symbol("vm-no-such-charset")]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("charsetp"),
                    Value::symbol("vm-no-such-charset")
                ]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Builtin tests: char-charset
// -----------------------------------------------------------------------

#[test]
fn char_charset_int() {
    crate::test_utils::init_test_tracing();
    let r = builtin_char_charset(vec![Value::fixnum(65)]).unwrap();
    assert!(r.is_symbol_named("ascii"));
}

#[test]
fn char_charset_char() {
    crate::test_utils::init_test_tracing();
    let r = builtin_char_charset(vec![Value::char('A')]).unwrap();
    assert!(r.is_symbol_named("ascii"));
}

#[test]
fn char_charset_with_restriction() {
    crate::test_utils::init_test_tracing();
    let r = builtin_char_charset(vec![Value::fixnum(65), Value::NIL]).unwrap();
    assert!(r.is_symbol_named("ascii"));
}

#[test]
fn char_charset_wrong_type() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_char_charset(vec![Value::string("not a char")]).is_err());
}

#[test]
fn char_charset_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_char_charset(vec![]).is_err());
    assert!(builtin_char_charset(vec![Value::fixnum(65), Value::NIL, Value::NIL]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-plist
// -----------------------------------------------------------------------

#[test]
fn charset_plist_known() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_plist(vec![Value::symbol("ascii")]).unwrap();
    // Standard charsets have empty plists.
    assert!(r.is_nil());
}

#[test]
fn charset_plist_unknown() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_plist(vec![Value::symbol("nonexistent")]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("charsetp"), Value::symbol("nonexistent")]
            );
        }
        other => panic!("expected wrong-type-argument charsetp, got {other:?}"),
    }
}

#[test]
fn charset_plist_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_charset_plist(vec![]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-id-internal
// -----------------------------------------------------------------------

#[test]
fn charset_id_internal_requires_charset() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_id_internal(vec![]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("charsetp"), Value::NIL]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn charset_id_internal_with_ascii() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_id_internal(vec![Value::symbol("ascii")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn charset_id_internal_with_unicode() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_id_internal(vec![Value::symbol("unicode")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn charset_id_internal_unknown_is_type_error() {
    crate::test_utils::init_test_tracing();
    let r = builtin_charset_id_internal(vec![Value::symbol("vm-no-such")]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("charsetp"), Value::symbol("vm-no-such")]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn charset_id_internal_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_charset_id_internal(vec![Value::NIL, Value::NIL]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: define-charset-internal
// -----------------------------------------------------------------------

#[test]
fn define_charset_internal_requires_exact_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_define_charset_internal(vec![]).is_err());
    assert!(builtin_define_charset_internal(vec![Value::NIL; 16]).is_err());
    assert!(builtin_define_charset_internal(vec![Value::NIL; 18]).is_err());
}

#[test]
fn define_charset_internal_validates_name_arg() {
    crate::test_utils::init_test_tracing();
    // arg[0] must be a symbol
    let err = builtin_define_charset_internal(vec![Value::NIL; 17]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::NIL]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn define_charset_internal_registers_charset() {
    crate::test_utils::init_test_tracing();
    let mut args = vec![Value::NIL; 17];
    args[0] = Value::symbol("test-charset-xyz");
    args[1] = Value::fixnum(1); // dimension
    args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(127)]); // code-space
    let r = builtin_define_charset_internal(args).unwrap();
    assert!(r.is_nil());
    // The charset should now be registered.
    let found = builtin_charsetp(vec![Value::symbol("test-charset-xyz")]).unwrap();
    assert!(found.is_t());
}

#[test]
fn define_charset_internal_keeps_symbol_plist_keys_and_roots_unify_map_value() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let unify_map = Value::string("8859-2");
    let mut args = vec![Value::NIL; 17];
    args[0] = Value::symbol("test-charset-live-metadata");
    args[1] = Value::fixnum(1);
    args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
    args[15] = unify_map;
    args[16] = Value::list(vec![
        Value::keyword(":foo"),
        Value::fixnum(42),
        Value::symbol("bar"),
        Value::T,
    ]);

    builtin_define_charset_internal(args).unwrap();

    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        let plist = reg
            .plist(intern("test-charset-live-metadata"))
            .expect("charset plist should exist");
        assert_eq!(plist.len(), 2);
        assert_eq!(resolve_sym(plist[0].0), ":foo");
        assert_eq!(plist[0].1, Value::fixnum(42));
        assert_eq!(resolve_sym(plist[1].0), "bar");
        assert_eq!(plist[1].1, Value::T);
    });

    let mut roots = Vec::new();
    collect_charset_gc_roots(&mut roots);
    assert!(roots.contains(&unify_map));
}

#[test]
fn define_charset_internal_short_code_space_signals_error() {
    crate::test_utils::init_test_tracing();
    let mut args = vec![Value::NIL; 17];
    args[0] = Value::symbol("test-short-cs");
    args[1] = Value::fixnum(1); // dimension
    args[2] = Value::vector(vec![Value::fixnum(0)]); // too short
    let err = builtin_define_charset_internal(args).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn charset_contains_char_supports_map_and_subset_charsets() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let mut parent_args = vec![Value::NIL; 17];
    parent_args[0] = Value::symbol("latin-iso8859-2-test");
    parent_args[1] = Value::fixnum(1);
    parent_args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
    parent_args[8] = Value::T;
    parent_args[12] = Value::string("8859-2");
    builtin_define_charset_internal(parent_args).unwrap();

    let mut subset_args = vec![Value::NIL; 17];
    subset_args[0] = Value::symbol("iso-8859-2-test");
    subset_args[1] = Value::fixnum(1);
    subset_args[2] = Value::vector(vec![Value::fixnum(32), Value::fixnum(127)]);
    subset_args[13] = Value::list(vec![
        Value::symbol("latin-iso8859-2-test"),
        Value::fixnum(160),
        Value::fixnum(255),
        Value::fixnum(-128),
    ]);
    builtin_define_charset_internal(subset_args).unwrap();

    assert_eq!(
        charset_contains_char("latin-iso8859-2-test", 'Ą' as u32),
        Some(true)
    );
    assert_eq!(
        charset_contains_char("latin-iso8859-2-test", '好' as u32),
        Some(false)
    );
    assert_eq!(
        charset_contains_char("iso-8859-2-test", 'Ą' as u32),
        Some(true)
    );
    assert_eq!(
        charset_contains_char("iso-8859-2-test", '好' as u32),
        Some(false)
    );
}

#[test]
fn charset_target_ranges_support_map_charsets() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let mut args = vec![Value::NIL; 17];
    args[0] = Value::symbol("latin-iso8859-2-test");
    args[1] = Value::fixnum(1);
    args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
    args[8] = Value::T;
    args[12] = Value::string("8859-2");
    builtin_define_charset_internal(args).unwrap();

    let ranges = charset_target_ranges("latin-iso8859-2-test").expect("map ranges");
    assert!(
        ranges
            .iter()
            .any(|(from, to)| ('Ą' as u32) >= *from && ('Ą' as u32) <= *to)
    );
}

#[test]
fn charset_superset_supports_offsets_membership_and_ranges() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let mut thai_args = vec![Value::NIL; 17];
    thai_args[0] = Value::symbol("thai-offset-test");
    thai_args[1] = Value::fixnum(1);
    thai_args[2] = Value::vector(vec![Value::fixnum(32), Value::fixnum(127)]);
    thai_args[11] = Value::fixnum(0x0E00);
    builtin_define_charset_internal(thai_args).unwrap();

    let superset_members = Value::list(vec![
        Value::symbol("ascii"),
        Value::cons(Value::symbol("thai-offset-test"), Value::fixnum(96)),
    ]);

    let mut superset_args = vec![Value::NIL; 17];
    superset_args[0] = Value::symbol("thai-ascii-superset-test");
    superset_args[1] = Value::fixnum(1);
    superset_args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
    superset_args[14] = superset_members;
    superset_args[16] = Value::list(vec![Value::symbol("superset"), superset_members]);
    builtin_define_charset_internal(superset_args).unwrap();

    let thai_ko_kai = 'ก' as i64;
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        let superset = intern("thai-ascii-superset-test");
        assert_eq!(reg.decode_char(superset, 65), Some('A' as i64));
        assert_eq!(reg.decode_char(superset, 129), Some(thai_ko_kai));
        assert_eq!(reg.encode_char(superset, 'A' as i64), Some(65));
        assert_eq!(reg.encode_char(superset, thai_ko_kai), Some(129));
    });

    assert_eq!(
        charset_contains_char("thai-ascii-superset-test", 'A' as u32),
        Some(true)
    );
    assert_eq!(
        charset_contains_char("thai-ascii-superset-test", 'ก' as u32),
        Some(true)
    );
    assert_eq!(
        charset_contains_char("thai-ascii-superset-test", '好' as u32),
        Some(false)
    );

    let ranges = charset_target_ranges("thai-ascii-superset-test").expect("superset ranges");
    assert!(
        ranges
            .iter()
            .any(|(from, to)| ('A' as u32) >= *from && ('A' as u32) <= *to)
    );
    assert!(
        ranges
            .iter()
            .any(|(from, to)| ('ก' as u32) >= *from && ('ก' as u32) <= *to)
    );
}

#[test]
fn offset_charset_decode_encode_and_make_char_match_gnu_code_index_mapping() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let mut thai_args = vec![Value::NIL; 17];
    thai_args[0] = Value::symbol("thai-tis620-test");
    thai_args[1] = Value::fixnum(1);
    thai_args[2] = Value::vector(vec![Value::fixnum(32), Value::fixnum(127)]);
    thai_args[5] = Value::fixnum('T' as i64);
    thai_args[11] = Value::fixnum(0x0E00);
    builtin_define_charset_internal(thai_args).unwrap();

    assert!(
        builtin_decode_char(vec![Value::symbol("thai-tis620-test"), Value::fixnum(230)])
            .expect("decode-char out of charset range should return nil")
            .is_nil()
    );
    assert_eq!(
        builtin_decode_char(vec![Value::symbol("thai-tis620-test"), Value::fixnum(0x66)])
            .expect("thai decode-char should evaluate"),
        Value::fixnum(0x0E46)
    );
    assert_eq!(
        builtin_encode_char(vec![
            Value::fixnum(0x0E46),
            Value::symbol("thai-tis620-test")
        ])
        .expect("thai encode-char should evaluate"),
        Value::fixnum(0x66)
    );
    assert_eq!(
        builtin_make_char(vec![Value::symbol("thai-tis620-test"), Value::fixnum(230),])
            .expect("thai make-char should apply ISO final masking"),
        Value::fixnum(0x0E46)
    );
    assert_eq!(
        builtin_make_char(vec![Value::symbol("thai-tis620-test")])
            .expect("thai make-char default should use minimum code"),
        Value::fixnum(0x0E00)
    );

    let out_of_range =
        builtin_make_char(vec![Value::symbol("thai-tis620-test"), Value::fixnum(256)])
            .expect_err("position codes above one byte should signal");
    match out_of_range {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "args-out-of-range");
            assert_eq!(sig.data, vec![Value::fixnum(0xff), Value::fixnum(256)]);
        }
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn charset_registry_plist_values_survive_exact_gc_and_pdump_dump() {
    crate::test_utils::init_test_tracing();
    reset_charset_registry();

    let mut eval = Context::new();

    let mut args = vec![Value::NIL; 17];
    args[0] = Value::symbol("charset-pdump-root-test");
    args[1] = Value::fixnum(1);
    args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(127)]);
    args[16] = Value::list(vec![
        Value::symbol("doc"),
        Value::string("charset plist string should stay live"),
    ]);
    builtin_define_charset_internal(args).unwrap();

    eval.gc_collect_exact();

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("charset-registry-root-test.pdump");
    dump_to_file(&eval, &dump_path).expect("pdump dump should keep charset plist strings alive");

    reset_charset_registry();
}

// -----------------------------------------------------------------------
// Builtin tests: find-charset-region
// -----------------------------------------------------------------------

#[test]
fn find_charset_region_ascii_default() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert(&"a".repeat(100));
    }
    let r =
        builtin_find_charset_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(100)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("ascii"));
}

#[test]
fn find_charset_region_with_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert(&"a".repeat(100));
    }
    let r = builtin_find_charset_region(
        &mut eval,
        vec![Value::fixnum(1), Value::fixnum(100), Value::NIL],
    )
    .unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn find_charset_region_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    assert!(builtin_find_charset_region(&mut eval, vec![Value::fixnum(1)]).is_err());
    assert!(
        builtin_find_charset_region(
            &mut eval,
            vec![Value::fixnum(1), Value::fixnum(2), Value::NIL, Value::NIL,]
        )
        .is_err()
    );
}

#[test]
fn find_charset_region_eval_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("aé😀");
    }

    let all = builtin_find_charset_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(4)])
        .expect("find-charset-region all");
    assert_eq!(
        all,
        Value::list(vec![
            Value::symbol("ascii"),
            Value::symbol("unicode"),
            Value::symbol("unicode-bmp"),
        ])
    );

    let bmp = builtin_find_charset_region(&mut eval, vec![Value::fixnum(2), Value::fixnum(3)])
        .expect("find-charset-region bmp");
    assert_eq!(bmp, Value::list(vec![Value::symbol("unicode-bmp")]));

    let empty = builtin_find_charset_region(&mut eval, vec![Value::fixnum(4), Value::fixnum(4)])
        .expect("find-charset-region empty");
    assert_eq!(empty, Value::list(vec![Value::symbol("ascii")]));
}

#[test]
fn find_charset_region_eval_out_of_range_errors() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc");
    }
    assert!(
        builtin_find_charset_region(&mut eval, vec![Value::fixnum(0), Value::fixnum(2)]).is_err()
    );
    assert!(
        builtin_find_charset_region(&mut eval, vec![Value::fixnum(1), Value::fixnum(5)]).is_err()
    );
}

// -----------------------------------------------------------------------
// Builtin tests: find-charset-string
// -----------------------------------------------------------------------

#[test]
fn find_charset_string_ascii() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::string("hello")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("ascii"));
}

#[test]
fn find_charset_string_empty_is_nil() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::string("")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn find_charset_string_bmp() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::string("é")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("unicode-bmp"));
}

#[test]
fn find_charset_string_unicode_supplementary() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::string("😀")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("unicode"));
}

#[test]
fn find_charset_string_mixed_order_matches_oracle() {
    crate::test_utils::init_test_tracing();
    let mut s = String::from("a😀é");
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let r = builtin_find_charset_string(vec![Value::string(s)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 4);
    assert!(items[0].is_symbol_named("ascii"));
    assert!(items[1].is_symbol_named("unicode"));
    assert!(items[2].is_symbol_named("eight-bit"));
    assert!(items[3].is_symbol_named("unicode-bmp"));
}

#[test]
fn find_charset_string_unibyte_ascii_and_eight_bit() {
    crate::test_utils::init_test_tracing();
    let mut s = String::new();
    s.push(char::from_u32(0xE341).expect("valid unibyte ascii sentinel"));
    s.push(char::from_u32(0xE3FF).expect("valid unibyte 255 sentinel"));
    let r = builtin_find_charset_string(vec![Value::string(s)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_symbol_named("ascii"));
    assert!(items[1].is_symbol_named("eight-bit"));
}

#[test]
fn find_charset_string_raw_unibyte_value_reports_eight_bit() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let r = builtin_find_charset_string(vec![raw]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_symbol_named("eight-bit"));
}

#[test]
fn find_charset_string_with_table() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::string("hello"), Value::NIL]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn find_charset_string_wrong_type() {
    crate::test_utils::init_test_tracing();
    let r = builtin_find_charset_string(vec![Value::fixnum(1)]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn find_charset_string_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_find_charset_string(vec![]).is_err());
    assert!(
        builtin_find_charset_string(vec![Value::string("a"), Value::NIL, Value::NIL,]).is_err()
    );
}

// -----------------------------------------------------------------------
// Builtin tests: decode-char
// -----------------------------------------------------------------------

#[test]
fn decode_char_ascii() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("ascii"), Value::fixnum(65)]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn decode_char_unicode() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::fixnum(0x1F600)]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn decode_char_eight_bit_maps_to_raw_byte_range() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("eight-bit"), Value::fixnum(255)]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn decode_char_invalid_code_point() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::fixnum(0xD800)]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn decode_char_negative() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::fixnum(-1)]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Not an in-range integer, integral float, or cons of integers"
                )]
            );
        }
        other => panic!("expected decode-char error signal, got {other:?}"),
    }
}

#[test]
fn decode_char_out_of_range() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::fixnum(0x110000)]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn decode_char_unknown_charset() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("nonexistent"), Value::fixnum(65)]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("charsetp"), Value::symbol("nonexistent")]
            );
        }
        other => panic!("expected wrong-type-argument charsetp, got {other:?}"),
    }
}

#[test]
fn decode_char_wrong_type() {
    crate::test_utils::init_test_tracing();
    let r = builtin_decode_char(vec![Value::symbol("ascii"), Value::string("not an int")]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Not an in-range integer, integral float, or cons of integers"
                )]
            );
        }
        other => panic!("expected decode-char type error, got {other:?}"),
    }
}

#[test]
fn decode_char_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_decode_char(vec![Value::symbol("ascii")]).is_err());
    assert!(
        builtin_decode_char(vec![Value::symbol("ascii"), Value::fixnum(65), Value::NIL]).is_err()
    );
}

// -----------------------------------------------------------------------
// Builtin tests: encode-char
// -----------------------------------------------------------------------

#[test]
fn encode_char_basic() {
    crate::test_utils::init_test_tracing();
    let r = builtin_encode_char(vec![Value::fixnum(65), Value::symbol("ascii")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn encode_char_unicode() {
    crate::test_utils::init_test_tracing();
    let r = builtin_encode_char(vec![Value::fixnum(0x1F600), Value::symbol("unicode")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn encode_char_eight_bit_raw_byte_maps_back_to_byte() {
    crate::test_utils::init_test_tracing();
    let r = builtin_encode_char(vec![Value::fixnum(0x3FFFFF), Value::symbol("eight-bit")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn encode_char_with_char_value() {
    crate::test_utils::init_test_tracing();
    let r = builtin_encode_char(vec![Value::char('Z'), Value::symbol("unicode")]).unwrap();
    assert!(r.is_fixnum());
}

#[test]
fn encode_char_unknown_charset() {
    crate::test_utils::init_test_tracing();
    let r = builtin_encode_char(vec![Value::fixnum(65), Value::symbol("nonexistent")]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("charsetp"), Value::symbol("nonexistent")]
            );
        }
        other => panic!("expected wrong-type-argument charsetp, got {other:?}"),
    }
}

#[test]
fn encode_char_wrong_type() {
    crate::test_utils::init_test_tracing();
    assert!(
        builtin_encode_char(vec![Value::string("not a char"), Value::symbol("ascii")]).is_err()
    );
}

#[test]
fn encode_char_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_encode_char(vec![Value::fixnum(65)]).is_err());
}

#[test]
fn encode_decode_big5_sjis_basic_identity() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_encode_big5_char(vec![Value::fixnum(65)]).expect("encode-big5-char"),
        Value::fixnum(65)
    );
    assert_eq!(
        builtin_decode_big5_char(vec![Value::fixnum(65)]).expect("decode-big5-char"),
        Value::fixnum(65)
    );
    assert_eq!(
        builtin_encode_sjis_char(vec![Value::fixnum(65)]).expect("encode-sjis-char"),
        Value::fixnum(65)
    );
    assert_eq!(
        builtin_decode_sjis_char(vec![Value::fixnum(65)]).expect("decode-sjis-char"),
        Value::fixnum(65)
    );
}

#[test]
fn get_unused_iso_final_char_known_values() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::fixnum(1), Value::fixnum(94)]).expect("1/94"),
        Value::fixnum(54)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::fixnum(1), Value::fixnum(96)]).expect("1/96"),
        Value::fixnum(51)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::fixnum(2), Value::fixnum(94)]).expect("2/94"),
        Value::fixnum(50)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::fixnum(2), Value::fixnum(96)]).expect("2/96"),
        Value::fixnum(48)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::fixnum(3), Value::fixnum(94)]).expect("3/94"),
        Value::fixnum(48)
    );
}

#[test]
fn get_unused_iso_final_char_validates_dimension_and_chars() {
    crate::test_utils::init_test_tracing();
    let bad_dimension =
        builtin_get_unused_iso_final_char(vec![Value::fixnum(0), Value::fixnum(94)])
            .expect_err("dimension 0 should error");
    assert!(matches!(bad_dimension, Flow::Signal(_)));

    let bad_chars = builtin_get_unused_iso_final_char(vec![Value::fixnum(1), Value::fixnum(0)])
        .expect_err("chars 0 should error");
    assert!(matches!(bad_chars, Flow::Signal(_)));
}

#[test]
fn declare_equiv_charset_validates_and_accepts_valid_tuple() {
    crate::test_utils::init_test_tracing();
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::fixnum(1),
            Value::fixnum(94),
            Value::fixnum(65),
            Value::symbol("ascii"),
        ])
        .is_ok()
    );

    assert!(
        builtin_declare_equiv_charset(vec![
            Value::fixnum(0),
            Value::fixnum(94),
            Value::fixnum(65),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::fixnum(1),
            Value::fixnum(0),
            Value::fixnum(65),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::fixnum(1),
            Value::fixnum(94),
            Value::symbol("A"),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
}

#[test]
fn define_charset_alias_adds_symbol_alias_only() {
    crate::test_utils::init_test_tracing();
    assert!(
        builtin_define_charset_alias(vec![
            Value::symbol("latin-1"),
            Value::symbol("latin-iso8859-1"),
        ])
        .is_ok()
    );
    assert!(
        builtin_charsetp(vec![Value::symbol("latin-1")])
            .expect("charsetp latin-1")
            .is_truthy()
    );
    assert_eq!(
        builtin_charset_id_internal(vec![Value::symbol("latin-1")]).expect("id latin-1"),
        Value::fixnum(5)
    );

    // Non-symbol aliases are accepted but do not register a new symbol alias.
    assert!(builtin_define_charset_alias(vec![Value::fixnum(1), Value::symbol("ascii")]).is_ok());
}

// -----------------------------------------------------------------------
// Builtin tests: clear-charset-maps
// -----------------------------------------------------------------------

#[test]
fn clear_charset_maps_returns_nil() {
    crate::test_utils::init_test_tracing();
    let r = builtin_clear_charset_maps(vec![]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn clear_charset_maps_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_clear_charset_maps(vec![Value::NIL]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-after
// -----------------------------------------------------------------------

#[test]
fn charset_after_default_returns_unicode() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("😀");
        buf.goto_char(buf.point_min());
    }
    let r = builtin_charset_after(&mut eval, vec![]).unwrap();
    assert!(r.is_symbol_named("unicode"));
}

#[test]
fn charset_after_with_pos() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("😀");
    }
    let r = builtin_charset_after(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(r.is_symbol_named("unicode"));
}

#[test]
fn charset_after_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    assert!(builtin_charset_after(&mut eval, vec![Value::fixnum(1), Value::fixnum(2)]).is_err());
}

#[test]
fn charset_after_eval_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("aé😀");
    }

    // No arg uses char after point; after insert point is at EOB.
    assert!(
        builtin_charset_after(&mut eval, vec![])
            .expect("charset-after no arg")
            .is_nil()
    );

    assert_eq!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(1)]).expect("pos 1"),
        Value::symbol("ascii")
    );
    assert_eq!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(2)]).expect("pos 2"),
        Value::symbol("unicode-bmp")
    );
    assert_eq!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(3)]).expect("pos 3"),
        Value::symbol("unicode")
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(4)])
            .expect("pos 4")
            .is_nil()
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(0)])
            .expect("pos 0")
            .is_nil()
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::fixnum(10)])
            .expect("pos 10")
            .is_nil()
    );
    assert!(builtin_charset_after(&mut eval, vec![Value::string("x")]).is_err());
}

// -----------------------------------------------------------------------
// Round-trip tests
// -----------------------------------------------------------------------

#[test]
fn decode_encode_round_trip() {
    crate::test_utils::init_test_tracing();
    // decode-char then encode-char should give the same code-point.
    let code = 0x00E9_i64; // e-acute
    let decoded = builtin_decode_char(vec![Value::symbol("unicode"), Value::fixnum(code)]).unwrap();
    let cp = decoded.as_int().unwrap();
    let encoded = builtin_encode_char(vec![Value::fixnum(cp), Value::symbol("unicode")]).unwrap();
    assert!(encoded.as_fixnum().map_or(false, |n| n == code));
}

#[test]
fn charsetp_all_standard() {
    crate::test_utils::init_test_tracing();
    for name in &[
        "ascii",
        "unicode",
        "unicode-bmp",
        "latin-iso8859-1",
        "emacs",
        "eight-bit",
    ] {
        let r = builtin_charsetp(vec![Value::symbol(*name)]).unwrap();
        assert!(r.is_t(), "charsetp should return t for {}", name);
    }
}
