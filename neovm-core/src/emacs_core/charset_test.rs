use super::*;

// -----------------------------------------------------------------------
// CharsetRegistry unit tests
// -----------------------------------------------------------------------

#[test]
fn registry_has_standard_charsets() {
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
    let reg = CharsetRegistry::new();
    let names = reg.names();
    assert_eq!(names.len(), 8);
    assert!(names.contains(&"ascii".to_string()));
    assert!(names.contains(&"unicode".to_string()));
}

#[test]
fn registry_priority_list() {
    let reg = CharsetRegistry::new();
    let prio = reg.priority_list();
    assert!(!prio.is_empty());
    // unicode should be the highest priority.
    assert_eq!(prio[0], "unicode");
}

#[test]
fn registry_plist_returns_empty_for_standard() {
    let reg = CharsetRegistry::new();
    let plist = reg.plist("ascii").unwrap();
    assert!(plist.is_empty());
}

#[test]
fn registry_plist_none_for_unknown() {
    let reg = CharsetRegistry::new();
    assert!(reg.plist("nonexistent").is_none());
}

// -----------------------------------------------------------------------
// Builtin tests: charsetp
// -----------------------------------------------------------------------

#[test]
fn charsetp_known() {
    let r = builtin_charsetp(vec![Value::symbol("ascii")]).unwrap();
    assert!(matches!(r, Value::True));
}

#[test]
fn charsetp_unknown() {
    let r = builtin_charsetp(vec![Value::symbol("nonexistent")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_string_arg() {
    let r = builtin_charsetp(vec![Value::string("unicode")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_non_symbol() {
    let r = builtin_charsetp(vec![Value::Int(42)]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn charsetp_wrong_arg_count() {
    assert!(builtin_charsetp(vec![]).is_err());
    assert!(builtin_charsetp(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn charset_list_returns_priority_order() {
    let r = builtin_charset_list(vec![]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "unicode"));
    assert!(items.len() >= 2);
}

#[test]
fn unibyte_charset_returns_eight_bit() {
    let r = builtin_unibyte_charset(vec![]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "eight-bit"));
}

// -----------------------------------------------------------------------
// Builtin tests: charset-priority-list
// -----------------------------------------------------------------------

#[test]
fn charset_priority_list_full() {
    let r = builtin_charset_priority_list(vec![]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(!items.is_empty());
    // First should be unicode.
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "unicode"));
}

#[test]
fn charset_priority_list_highestp() {
    let r = builtin_charset_priority_list(vec![Value::True]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "unicode"));
}

#[test]
fn charset_priority_list_highestp_nil() {
    let r = builtin_charset_priority_list(vec![Value::Nil]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert!(items.len() > 1);
}

// -----------------------------------------------------------------------
// Builtin tests: set-charset-priority
// -----------------------------------------------------------------------

#[test]
fn registry_set_priority_reorders_and_dedups() {
    let mut reg = CharsetRegistry::new();
    reg.set_priority(&[
        "ascii".to_string(),
        "unicode".to_string(),
        "ascii".to_string(),
    ]);
    assert_eq!(reg.priority[0], "ascii");
    assert_eq!(reg.priority[1], "unicode");
}

#[test]
fn set_charset_priority_requires_at_least_one_arg() {
    assert!(builtin_set_charset_priority(vec![]).is_err());
}

#[test]
fn set_charset_priority_rejects_unknown_charset() {
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
    let r = builtin_char_charset(vec![Value::Int(65)]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "ascii"));
}

#[test]
fn char_charset_char() {
    let r = builtin_char_charset(vec![Value::Char('A')]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "ascii"));
}

#[test]
fn char_charset_with_restriction() {
    let r = builtin_char_charset(vec![Value::Int(65), Value::Nil]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "ascii"));
}

#[test]
fn char_charset_wrong_type() {
    assert!(builtin_char_charset(vec![Value::string("not a char")]).is_err());
}

#[test]
fn char_charset_wrong_arg_count() {
    assert!(builtin_char_charset(vec![]).is_err());
    assert!(builtin_char_charset(vec![Value::Int(65), Value::Nil, Value::Nil]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-plist
// -----------------------------------------------------------------------

#[test]
fn charset_plist_known() {
    let r = builtin_charset_plist(vec![Value::symbol("ascii")]).unwrap();
    // Standard charsets have empty plists.
    assert!(r.is_nil());
}

#[test]
fn charset_plist_unknown() {
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
    assert!(builtin_charset_plist(vec![]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-id-internal
// -----------------------------------------------------------------------

#[test]
fn charset_id_internal_requires_charset() {
    let r = builtin_charset_id_internal(vec![]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("charsetp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn charset_id_internal_with_ascii() {
    let r = builtin_charset_id_internal(vec![Value::symbol("ascii")]).unwrap();
    assert!(matches!(r, Value::Int(0)));
}

#[test]
fn charset_id_internal_with_unicode() {
    let r = builtin_charset_id_internal(vec![Value::symbol("unicode")]).unwrap();
    assert!(matches!(r, Value::Int(2)));
}

#[test]
fn charset_id_internal_unknown_is_type_error() {
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
    assert!(builtin_charset_id_internal(vec![Value::Nil, Value::Nil]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: define-charset-internal
// -----------------------------------------------------------------------

#[test]
fn define_charset_internal_requires_exact_arity() {
    assert!(builtin_define_charset_internal(vec![]).is_err());
    assert!(builtin_define_charset_internal(vec![Value::Nil; 16]).is_err());
    assert!(builtin_define_charset_internal(vec![Value::Nil; 18]).is_err());
}

#[test]
fn define_charset_internal_validates_name_arg() {
    // arg[0] must be a symbol
    let err = builtin_define_charset_internal(vec![Value::Nil; 17]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn define_charset_internal_registers_charset() {
    let mut args = vec![Value::Nil; 17];
    args[0] = Value::symbol("test-charset-xyz");
    args[1] = Value::Int(1); // dimension
    args[2] = Value::vector(vec![Value::Int(0), Value::Int(127)]); // code-space
    let r = builtin_define_charset_internal(args).unwrap();
    assert!(r.is_nil());
    // The charset should now be registered.
    let found = builtin_charsetp(vec![Value::symbol("test-charset-xyz")]).unwrap();
    assert!(matches!(found, Value::True));
}

#[test]
fn define_charset_internal_short_code_space_signals_error() {
    let mut args = vec![Value::Nil; 17];
    args[0] = Value::symbol("test-short-cs");
    args[1] = Value::Int(1); // dimension
    args[2] = Value::vector(vec![Value::Int(0)]); // too short
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
    reset_charset_registry();

    let mut parent_args = vec![Value::Nil; 17];
    parent_args[0] = Value::symbol("latin-iso8859-2-test");
    parent_args[1] = Value::Int(1);
    parent_args[2] = Value::vector(vec![Value::Int(0), Value::Int(255)]);
    parent_args[8] = Value::True;
    parent_args[12] = Value::string("8859-2");
    builtin_define_charset_internal(parent_args).unwrap();

    let mut subset_args = vec![Value::Nil; 17];
    subset_args[0] = Value::symbol("iso-8859-2-test");
    subset_args[1] = Value::Int(1);
    subset_args[2] = Value::vector(vec![Value::Int(32), Value::Int(127)]);
    subset_args[13] = Value::list(vec![
        Value::symbol("latin-iso8859-2-test"),
        Value::Int(160),
        Value::Int(255),
        Value::Int(-128),
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
    reset_charset_registry();

    let mut args = vec![Value::Nil; 17];
    args[0] = Value::symbol("latin-iso8859-2-test");
    args[1] = Value::Int(1);
    args[2] = Value::vector(vec![Value::Int(0), Value::Int(255)]);
    args[8] = Value::True;
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
    reset_charset_registry();

    let mut thai_args = vec![Value::Nil; 17];
    thai_args[0] = Value::symbol("thai-offset-test");
    thai_args[1] = Value::Int(1);
    thai_args[2] = Value::vector(vec![Value::Int(0), Value::Int(127)]);
    thai_args[8] = Value::True;
    thai_args[11] = Value::Int(0x0E00);
    builtin_define_charset_internal(thai_args).unwrap();

    let superset_members = Value::list(vec![
        Value::symbol("ascii"),
        Value::cons(Value::symbol("thai-offset-test"), Value::Int(128)),
    ]);

    let mut superset_args = vec![Value::Nil; 17];
    superset_args[0] = Value::symbol("thai-ascii-superset-test");
    superset_args[1] = Value::Int(1);
    superset_args[2] = Value::vector(vec![Value::Int(0), Value::Int(255)]);
    superset_args[14] = superset_members;
    superset_args[16] = Value::list(vec![Value::symbol("superset"), superset_members]);
    builtin_define_charset_internal(superset_args).unwrap();

    let thai_ko_kai = 'ก' as i64;
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        assert_eq!(
            reg.decode_char("thai-ascii-superset-test", 65),
            Some('A' as i64)
        );
        assert_eq!(
            reg.decode_char("thai-ascii-superset-test", 129),
            Some(thai_ko_kai)
        );
        assert_eq!(
            reg.encode_char("thai-ascii-superset-test", 'A' as i64),
            Some(65)
        );
        assert_eq!(
            reg.encode_char("thai-ascii-superset-test", thai_ko_kai),
            Some(129)
        );
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

// -----------------------------------------------------------------------
// Builtin tests: find-charset-region
// -----------------------------------------------------------------------

#[test]
fn find_charset_region_ascii_default() {
    let r = builtin_find_charset_region_inner(vec![Value::Int(1), Value::Int(100)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "ascii"));
}

#[test]
fn find_charset_region_with_table() {
    let r = builtin_find_charset_region_inner(vec![Value::Int(1), Value::Int(100), Value::Nil]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn find_charset_region_wrong_arg_count() {
    assert!(builtin_find_charset_region_inner(vec![Value::Int(1)]).is_err());
    assert!(
        builtin_find_charset_region_inner(vec![Value::Int(1), Value::Int(2), Value::Nil, Value::Nil,])
            .is_err()
    );
}

#[test]
fn find_charset_region_eval_semantics() {
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("aé😀");
    }

    let all = builtin_find_charset_region(&mut eval, vec![Value::Int(1), Value::Int(4)])
        .expect("find-charset-region all");
    assert_eq!(
        all,
        Value::list(vec![
            Value::symbol("ascii"),
            Value::symbol("unicode"),
            Value::symbol("unicode-bmp"),
        ])
    );

    let bmp = builtin_find_charset_region(&mut eval, vec![Value::Int(2), Value::Int(3)])
        .expect("find-charset-region bmp");
    assert_eq!(bmp, Value::list(vec![Value::symbol("unicode-bmp")]));

    let empty = builtin_find_charset_region(&mut eval, vec![Value::Int(4), Value::Int(4)])
        .expect("find-charset-region empty");
    assert_eq!(empty, Value::list(vec![Value::symbol("ascii")]));
}

#[test]
fn find_charset_region_eval_out_of_range_errors() {
    let mut eval = super::super::eval::Context::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc");
    }
    assert!(
        builtin_find_charset_region(&mut eval, vec![Value::Int(0), Value::Int(2)]).is_err()
    );
    assert!(
        builtin_find_charset_region(&mut eval, vec![Value::Int(1), Value::Int(5)]).is_err()
    );
}

// -----------------------------------------------------------------------
// Builtin tests: find-charset-string
// -----------------------------------------------------------------------

#[test]
fn find_charset_string_ascii() {
    let r = builtin_find_charset_string(vec![Value::string("hello")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "ascii"));
}

#[test]
fn find_charset_string_empty_is_nil() {
    let r = builtin_find_charset_string(vec![Value::string("")]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn find_charset_string_bmp() {
    let r = builtin_find_charset_string(vec![Value::string("é")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "unicode-bmp"));
}

#[test]
fn find_charset_string_unicode_supplementary() {
    let r = builtin_find_charset_string(vec![Value::string("😀")]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "unicode"));
}

#[test]
fn find_charset_string_mixed_order_matches_oracle() {
    let mut s = String::from("a😀é");
    s.push(char::from_u32(0xE3FF).expect("valid unibyte sentinel"));
    let r = builtin_find_charset_string(vec![Value::string(s)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 4);
    assert!(matches!(&items[0], Value::Symbol(v) if resolve_sym(*v) == "ascii"));
    assert!(matches!(&items[1], Value::Symbol(v) if resolve_sym(*v) == "unicode"));
    assert!(matches!(&items[2], Value::Symbol(v) if resolve_sym(*v) == "eight-bit"));
    assert!(matches!(&items[3], Value::Symbol(v) if resolve_sym(*v) == "unicode-bmp"));
}

#[test]
fn find_charset_string_unibyte_ascii_and_eight_bit() {
    let mut s = String::new();
    s.push(char::from_u32(0xE341).expect("valid unibyte ascii sentinel"));
    s.push(char::from_u32(0xE3FF).expect("valid unibyte 255 sentinel"));
    let r = builtin_find_charset_string(vec![Value::string(s)]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], Value::Symbol(v) if resolve_sym(*v) == "ascii"));
    assert!(matches!(&items[1], Value::Symbol(v) if resolve_sym(*v) == "eight-bit"));
}

#[test]
fn find_charset_string_with_table() {
    let r = builtin_find_charset_string(vec![Value::string("hello"), Value::Nil]).unwrap();
    let items = list_to_vec(&r).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn find_charset_string_wrong_type() {
    let r = builtin_find_charset_string(vec![Value::Int(1)]);
    match r {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn find_charset_string_wrong_arg_count() {
    assert!(builtin_find_charset_string(vec![]).is_err());
    assert!(
        builtin_find_charset_string(vec![Value::string("a"), Value::Nil, Value::Nil,]).is_err()
    );
}

// -----------------------------------------------------------------------
// Builtin tests: decode-char
// -----------------------------------------------------------------------

#[test]
fn decode_char_ascii() {
    let r = builtin_decode_char(vec![Value::symbol("ascii"), Value::Int(65)]).unwrap();
    assert!(matches!(r, Value::Int(65)));
}

#[test]
fn decode_char_unicode() {
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::Int(0x1F600)]).unwrap();
    assert!(matches!(r, Value::Int(0x1F600)));
}

#[test]
fn decode_char_eight_bit_maps_to_raw_byte_range() {
    let r = builtin_decode_char(vec![Value::symbol("eight-bit"), Value::Int(255)]).unwrap();
    assert!(matches!(r, Value::Int(0x3FFFFF)));
}

#[test]
fn decode_char_invalid_code_point() {
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::Int(0xD800)]).unwrap();
    assert!(matches!(r, Value::Int(0xD800)));
}

#[test]
fn decode_char_negative() {
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::Int(-1)]);
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
    let r = builtin_decode_char(vec![Value::symbol("unicode"), Value::Int(0x110000)]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn decode_char_unknown_charset() {
    let r = builtin_decode_char(vec![Value::symbol("nonexistent"), Value::Int(65)]);
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
    assert!(builtin_decode_char(vec![Value::symbol("ascii")]).is_err());
    assert!(builtin_decode_char(vec![Value::symbol("ascii"), Value::Int(65), Value::Nil]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: encode-char
// -----------------------------------------------------------------------

#[test]
fn encode_char_basic() {
    let r = builtin_encode_char(vec![Value::Int(65), Value::symbol("ascii")]).unwrap();
    assert!(matches!(r, Value::Int(65)));
}

#[test]
fn encode_char_unicode() {
    let r = builtin_encode_char(vec![Value::Int(0x1F600), Value::symbol("unicode")]).unwrap();
    assert!(matches!(r, Value::Int(0x1F600)));
}

#[test]
fn encode_char_eight_bit_raw_byte_maps_back_to_byte() {
    let r = builtin_encode_char(vec![Value::Int(0x3FFFFF), Value::symbol("eight-bit")]).unwrap();
    assert!(matches!(r, Value::Int(255)));
}

#[test]
fn encode_char_with_char_value() {
    let r = builtin_encode_char(vec![Value::Char('Z'), Value::symbol("unicode")]).unwrap();
    assert!(matches!(r, Value::Int(90)));
}

#[test]
fn encode_char_unknown_charset() {
    let r = builtin_encode_char(vec![Value::Int(65), Value::symbol("nonexistent")]);
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
    assert!(
        builtin_encode_char(vec![Value::string("not a char"), Value::symbol("ascii")]).is_err()
    );
}

#[test]
fn encode_char_wrong_arg_count() {
    assert!(builtin_encode_char(vec![Value::Int(65)]).is_err());
}

#[test]
fn encode_decode_big5_sjis_basic_identity() {
    assert_eq!(
        builtin_encode_big5_char(vec![Value::Int(65)]).expect("encode-big5-char"),
        Value::Int(65)
    );
    assert_eq!(
        builtin_decode_big5_char(vec![Value::Int(65)]).expect("decode-big5-char"),
        Value::Int(65)
    );
    assert_eq!(
        builtin_encode_sjis_char(vec![Value::Int(65)]).expect("encode-sjis-char"),
        Value::Int(65)
    );
    assert_eq!(
        builtin_decode_sjis_char(vec![Value::Int(65)]).expect("decode-sjis-char"),
        Value::Int(65)
    );
}

#[test]
fn get_unused_iso_final_char_known_values() {
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::Int(1), Value::Int(94)]).expect("1/94"),
        Value::Int(54)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::Int(1), Value::Int(96)]).expect("1/96"),
        Value::Int(51)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::Int(2), Value::Int(94)]).expect("2/94"),
        Value::Int(50)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::Int(2), Value::Int(96)]).expect("2/96"),
        Value::Int(48)
    );
    assert_eq!(
        builtin_get_unused_iso_final_char(vec![Value::Int(3), Value::Int(94)]).expect("3/94"),
        Value::Int(48)
    );
}

#[test]
fn get_unused_iso_final_char_validates_dimension_and_chars() {
    let bad_dimension = builtin_get_unused_iso_final_char(vec![Value::Int(0), Value::Int(94)])
        .expect_err("dimension 0 should error");
    assert!(matches!(bad_dimension, Flow::Signal(_)));

    let bad_chars = builtin_get_unused_iso_final_char(vec![Value::Int(1), Value::Int(0)])
        .expect_err("chars 0 should error");
    assert!(matches!(bad_chars, Flow::Signal(_)));
}

#[test]
fn declare_equiv_charset_validates_and_accepts_valid_tuple() {
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::Int(1),
            Value::Int(94),
            Value::Int(65),
            Value::symbol("ascii"),
        ])
        .is_ok()
    );

    assert!(
        builtin_declare_equiv_charset(vec![
            Value::Int(0),
            Value::Int(94),
            Value::Int(65),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::Int(1),
            Value::Int(0),
            Value::Int(65),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
    assert!(
        builtin_declare_equiv_charset(vec![
            Value::Int(1),
            Value::Int(94),
            Value::symbol("A"),
            Value::symbol("ascii"),
        ])
        .is_err()
    );
}

#[test]
fn define_charset_alias_adds_symbol_alias_only() {
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
        Value::Int(5)
    );

    // Non-symbol aliases are accepted but do not register a new symbol alias.
    assert!(builtin_define_charset_alias(vec![Value::Int(1), Value::symbol("ascii")]).is_ok());
}

// -----------------------------------------------------------------------
// Builtin tests: clear-charset-maps
// -----------------------------------------------------------------------

#[test]
fn clear_charset_maps_returns_nil() {
    let r = builtin_clear_charset_maps(vec![]).unwrap();
    assert!(r.is_nil());
}

#[test]
fn clear_charset_maps_wrong_arg_count() {
    assert!(builtin_clear_charset_maps(vec![Value::Nil]).is_err());
}

// -----------------------------------------------------------------------
// Builtin tests: charset-after
// -----------------------------------------------------------------------

#[test]
fn charset_after_default_returns_unicode() {
    let r = builtin_charset_after_inner(vec![]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "unicode"));
}

#[test]
fn charset_after_with_pos() {
    let r = builtin_charset_after_inner(vec![Value::Int(42)]).unwrap();
    assert!(matches!(r, Value::Symbol(id) if resolve_sym(id) == "unicode"));
}

#[test]
fn charset_after_wrong_arg_count() {
    assert!(builtin_charset_after_inner(vec![Value::Int(1), Value::Int(2)]).is_err());
}

#[test]
fn charset_after_eval_semantics() {
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
        builtin_charset_after(&mut eval, vec![Value::Int(1)]).expect("pos 1"),
        Value::symbol("ascii")
    );
    assert_eq!(
        builtin_charset_after(&mut eval, vec![Value::Int(2)]).expect("pos 2"),
        Value::symbol("unicode-bmp")
    );
    assert_eq!(
        builtin_charset_after(&mut eval, vec![Value::Int(3)]).expect("pos 3"),
        Value::symbol("unicode")
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::Int(4)])
            .expect("pos 4")
            .is_nil()
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::Int(0)])
            .expect("pos 0")
            .is_nil()
    );
    assert!(
        builtin_charset_after(&mut eval, vec![Value::Int(10)])
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
    // decode-char then encode-char should give the same code-point.
    let code = 0x00E9_i64; // e-acute
    let decoded = builtin_decode_char(vec![Value::symbol("unicode"), Value::Int(code)]).unwrap();
    let cp = decoded.as_int().unwrap();
    let encoded = builtin_encode_char(vec![Value::Int(cp), Value::symbol("unicode")]).unwrap();
    assert!(matches!(encoded, Value::Int(n) if n == code));
}

#[test]
fn charsetp_all_standard() {
    for name in &[
        "ascii",
        "unicode",
        "unicode-bmp",
        "latin-iso8859-1",
        "emacs",
        "eight-bit",
    ] {
        let r = builtin_charsetp(vec![Value::symbol(*name)]).unwrap();
        assert!(
            matches!(r, Value::True),
            "charsetp should return t for {}",
            name
        );
    }
}
