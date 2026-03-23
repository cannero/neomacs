use super::*;

fn mgr() -> CodingSystemManager {
    CodingSystemManager::new()
}

fn mgr_with_latin9() -> CodingSystemManager {
    let mut m = mgr();
    m.register(CodingSystemInfo::new(
        "iso-latin-9",
        "charset",
        '0',
        EolType::Undecided,
    ));
    m.register(CodingSystemInfo::new(
        "iso-latin-9-unix",
        "charset",
        '0',
        EolType::Unix,
    ));
    m.register(CodingSystemInfo::new(
        "iso-latin-9-dos",
        "charset",
        '0',
        EolType::Dos,
    ));
    m.register(CodingSystemInfo::new(
        "iso-latin-9-mac",
        "charset",
        '0',
        EolType::Mac,
    ));
    m.add_alias("iso-8859-15", "iso-latin-9");
    m.add_alias("latin-9", "iso-latin-9");
    m.add_alias("latin-0", "iso-latin-9");
    m
}

fn plist_get(value: &Value, key: &str) -> Option<Value> {
    let needle = key.trim_start_matches(':');
    let items = list_to_vec(value)?;
    let mut idx = 0;
    while idx + 1 < items.len() {
        if items[idx]
            .as_symbol_name()
            .is_some_and(|name| name.trim_start_matches(':') == needle)
        {
            return Some(items[idx + 1]);
        }
        idx += 2;
    }
    None
}

// ----- CodingSystemManager construction -----

#[test]
fn new_manager_has_standard_systems() {
    let m = mgr();
    assert!(m.is_known("utf-8"));
    assert!(m.is_known("utf-8-unix"));
    assert!(m.is_known("utf-8-dos"));
    assert!(m.is_known("utf-8-mac"));
    assert!(m.is_known("latin-1"));
    assert!(m.is_known("ascii"));
    assert!(m.is_known("binary"));
    assert!(m.is_known("raw-text"));
    assert!(m.is_known("undecided"));
    assert!(m.is_known("emacs-internal"));
    assert!(m.is_known("no-conversion"));
    assert!(m.is_known("iso-latin-9"));
    assert!(m.is_known("iso-latin-9-unix"));
    assert!(m.is_known("iso-8859-15"));
    assert!(m.is_known("latin-9"));
}

#[test]
fn aliases_resolve() {
    let m = mgr();
    assert!(m.is_known("iso-8859-1")); // alias for latin-1
    assert!(m.is_known("iso-8859-15")); // alias for latin-9
    assert!(m.is_known("us-ascii")); // alias for ascii
    assert!(m.is_known("mule-utf-8")); // alias for utf-8
    assert_eq!(m.resolve("iso-8859-1"), Some("iso-latin-1"));
    assert_eq!(m.resolve("iso-8859-15"), Some("iso-latin-9"));
    assert_eq!(m.resolve("ascii"), Some("us-ascii"));
}

#[test]
fn unknown_system_not_known() {
    let m = mgr();
    assert!(!m.is_known("martian-encoding"));
    assert_eq!(m.resolve("martian-encoding"), None);
}

#[test]
fn add_alias_works() {
    let mut m = mgr();
    m.add_alias("my-utf8", "utf-8");
    assert!(m.is_known("my-utf8"));
    assert_eq!(m.resolve("my-utf8"), Some("utf-8"));
}

// ----- CodingSystemInfo -----

#[test]
fn base_name_strips_suffix() {
    let info = CodingSystemInfo::new("utf-8-unix", "utf-8", 'U', EolType::Unix);
    assert_eq!(info.base_name(), "utf-8");

    let info2 = CodingSystemInfo::new("utf-8", "utf-8", 'U', EolType::Undecided);
    assert_eq!(info2.base_name(), "utf-8");
}

// ----- coding-system-list -----

#[test]
fn coding_system_list_all() {
    let m = mgr();
    let result = builtin_coding_system_list(&m, vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert!(items.len() >= 11); // at least the 11 pre-registered systems
}

#[test]
fn coding_system_list_base_only() {
    let m = mgr();
    let result = builtin_coding_system_list(&m, vec![Value::True]).unwrap();
    let items = list_to_vec(&result).unwrap();
    // Should not contain utf-8-unix, utf-8-dos, utf-8-mac
    for item in &items {
        if let Value::Symbol(id) = item {
            let s = resolve_sym(*id);
            assert!(
                !s.ends_with("-unix") && !s.ends_with("-dos") && !s.ends_with("-mac"),
                "base-only list should not contain: {}",
                s
            );
        }
    }
}

#[test]
fn coding_system_list_rejects_too_many_args() {
    let m = mgr();
    let result = builtin_coding_system_list(&m, vec![Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

// ----- coding-system-aliases -----

#[test]
fn coding_system_aliases_found() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![Value::symbol("utf-8")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    // First element should be the canonical name
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
    // Should include aliases like mule-utf-8
    assert!(items.len() > 1);
}

#[test]
fn coding_system_aliases_derive_eol_suffixes() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![Value::symbol("latin-1-unix")]).unwrap();
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("iso-latin-1-unix"),
            Value::symbol("iso-8859-1-unix"),
            Value::symbol("latin-1-unix"),
        ])
    );
}

#[test]
fn coding_system_aliases_unknown() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![Value::symbol("nonexistent")]);
    assert!(result.is_err());
}

#[test]
fn coding_system_aliases_nil_maps_to_no_conversion_family() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![Value::Nil]).unwrap();
    assert_eq!(
        result,
        Value::list(vec![
            Value::symbol("no-conversion"),
            Value::symbol("binary")
        ])
    );
}

#[test]
fn coding_system_aliases_string_is_type_error() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![Value::string("utf-8")]);
    assert!(result.is_err());
}

// ----- coding-system-get -----

#[test]
fn coding_system_get_name() {
    let m = mgr();
    let result =
        builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":name")])
            .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
}

#[test]
fn coding_system_get_type() {
    let m = mgr();
    let result = builtin_coding_system_get(
        &m,
        vec![Value::symbol("latin-1"), Value::symbol(":coding-type")],
    )
    .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "charset"));
}

#[test]
fn coding_system_get_mnemonic() {
    let m = mgr();
    let result =
        builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":mnemonic")])
            .unwrap();
    assert!(eq_value(&result, &Value::Int('U' as i64)));
}

#[test]
fn coding_system_get_eol_type() {
    let m = mgr();
    let result = builtin_coding_system_get(
        &m,
        vec![Value::symbol("utf-8-unix"), Value::symbol(":eol-type")],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn coding_system_get_unknown_prop() {
    let m = mgr();
    let result = builtin_coding_system_get(
        &m,
        vec![Value::symbol("utf-8"), Value::symbol(":nonexistent")],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn coding_system_get_unknown_system() {
    let m = mgr();
    let result =
        builtin_coding_system_get(&m, vec![Value::symbol("bogus"), Value::symbol(":name")]);
    assert!(result.is_err());
}

// ----- coding-system-plist -----

#[test]
fn coding_system_plist_utf8_core_fields() {
    let m = mgr();
    let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();
    assert_eq!(plist_get(&plist, ":name"), Some(Value::symbol("utf-8")));
    assert_eq!(
        plist_get(&plist, ":coding-type"),
        Some(Value::symbol("utf-8"))
    );
    assert_eq!(plist_get(&plist, ":mnemonic"), Some(Value::Int('U' as i64)));
}

#[test]
fn coding_system_plist_keyword_keys_work_with_builtin_plist_get() {
    let m = mgr();
    let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();

    let name = crate::emacs_core::builtins::builtin_plist_get(vec![plist, Value::keyword(":name")])
        .unwrap();
    assert_eq!(name, Value::symbol("utf-8"));

    let mnemonic =
        crate::emacs_core::builtins::builtin_plist_get(vec![plist, Value::keyword(":mnemonic")])
            .unwrap();
    assert_eq!(mnemonic, Value::Int('U' as i64));
}

#[test]
fn coding_system_plist_normalizes_alias_and_eol_variant_name() {
    let m = mgr();
    let latin = builtin_coding_system_plist(&m, vec![Value::symbol("latin-1")]).unwrap();
    assert_eq!(
        plist_get(&latin, ":name"),
        Some(Value::symbol("iso-latin-1"))
    );

    let utf8_unix = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
    assert_eq!(plist_get(&utf8_unix, ":name"), Some(Value::symbol("utf-8")));
}

#[test]
fn coding_system_plist_nil_maps_to_no_conversion() {
    let m = mgr();
    let plist = builtin_coding_system_plist(&m, vec![Value::Nil]).unwrap();
    assert_eq!(
        plist_get(&plist, ":name"),
        Some(Value::symbol("no-conversion"))
    );
    assert_eq!(
        plist_get(&plist, ":coding-type"),
        Some(Value::symbol("raw-text"))
    );
}

#[test]
fn coding_system_plist_type_and_unknown_errors() {
    let m = mgr();
    let type_err = builtin_coding_system_plist(&m, vec![Value::string("utf-8")]);
    assert!(type_err.is_err());

    let unknown = builtin_coding_system_plist(&m, vec![Value::symbol("bogus")]);
    assert!(unknown.is_err());
}

#[test]
fn coding_system_plist_includes_custom_properties_from_put() {
    let mut m = mgr();
    builtin_coding_system_put(
        &mut m,
        vec![
            Value::symbol("utf-8"),
            Value::symbol(":foo"),
            Value::Int(42),
        ],
    )
    .unwrap();

    let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();
    assert_eq!(plist_get(&plist, ":foo"), Some(Value::Int(42)));
}

// ----- coding-system-put -----

#[test]
fn coding_system_put_custom_prop() {
    let mut m = mgr();
    let result = builtin_coding_system_put(
        &mut m,
        vec![
            Value::symbol("utf-8"),
            Value::symbol(":charset-list"),
            Value::list(vec![Value::symbol("unicode")]),
        ],
    )
    .unwrap();
    assert_eq!(result, Value::list(vec![Value::symbol("unicode")]));

    // Verify it was stored
    let get_result = builtin_coding_system_get(
        &m,
        vec![Value::symbol("utf-8"), Value::symbol(":charset-list")],
    )
    .unwrap();
    assert!(!get_result.is_nil());
}

#[test]
fn coding_system_put_mnemonic() {
    let mut m = mgr();
    builtin_coding_system_put(
        &mut m,
        vec![
            Value::symbol("utf-8"),
            Value::symbol(":mnemonic"),
            Value::Char('X'),
        ],
    )
    .unwrap();

    let result =
        builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":mnemonic")])
            .unwrap();
    assert!(eq_value(&result, &Value::Int('X' as i64)));
}

#[test]
fn coding_system_put_unknown_system_errors() {
    let mut m = mgr();
    let result = builtin_coding_system_put(
        &mut m,
        vec![Value::symbol("bogus"), Value::symbol(":foo"), Value::Int(1)],
    );
    assert!(result.is_err());
}

// ----- coding-system-base -----

#[test]
fn coding_system_base_with_suffix() {
    let m = mgr();
    let result = builtin_coding_system_base(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
}

#[test]
fn coding_system_base_without_suffix() {
    let m = mgr();
    let result = builtin_coding_system_base(&m, vec![Value::symbol("utf-8")]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
}

#[test]
fn coding_system_base_unknown_still_strips() {
    let m = mgr();
    let result = builtin_coding_system_base(&m, vec![Value::symbol("foo-bar-unix")]);
    assert!(result.is_err());
}

// ----- coding-system-eol-type -----

#[test]
fn eol_type_unix() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
    assert!(eq_value(&result, &Value::Int(0)));
}

#[test]
fn eol_type_dos() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-dos")]).unwrap();
    assert!(eq_value(&result, &Value::Int(1)));
}

#[test]
fn eol_type_mac() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-mac")]).unwrap();
    assert!(eq_value(&result, &Value::Int(2)));
}

#[test]
fn eol_type_undecided_returns_vector() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8")]).unwrap();
    // Should be a vector of [utf-8-unix utf-8-dos utf-8-mac]
    if let Value::Vector(v) = result {
        let locked = with_heap(|h| h.get_vector(v).clone());
        assert_eq!(locked.len(), 3);
        assert!(matches!(&locked[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8-unix"));
        assert!(matches!(&locked[1], Value::Symbol(id) if resolve_sym(*id) == "utf-8-dos"));
        assert!(matches!(&locked[2], Value::Symbol(id) if resolve_sym(*id) == "utf-8-mac"));
    } else {
        panic!("expected vector for undecided eol-type");
    }
}

#[test]
fn eol_type_latin_alias_uses_iso_latin_display_variants() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("latin-1")]).unwrap();
    if let Value::Vector(v) = result {
        let locked = with_heap(|h| h.get_vector(v).clone());
        assert_eq!(locked.len(), 3);
        assert_eq!(locked[0], Value::symbol("iso-latin-1-unix"));
        assert_eq!(locked[1], Value::symbol("iso-latin-1-dos"));
        assert_eq!(locked[2], Value::symbol("iso-latin-1-mac"));
    } else {
        panic!("expected vector for undecided latin-1 eol-type");
    }
}

#[test]
fn eol_type_nil_maps_to_no_conversion() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::Nil]).unwrap();
    assert_eq!(result, Value::Int(0));
}

#[test]
fn eol_type_non_symbol_designator_returns_nil() {
    let m = mgr();
    assert!(
        builtin_coding_system_eol_type(&m, vec![Value::string("utf-8")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_coding_system_eol_type(&m, vec![Value::Int(1)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn eol_type_unknown_returns_nil() {
    let m = mgr();
    let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("nonexistent")]).unwrap();
    assert!(result.is_nil());
}

// ----- coding-system-type -----

#[test]
fn coding_system_type_utf8() {
    let m = mgr();
    let result = builtin_coding_system_type(&m, vec![Value::symbol("utf-8")]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
}

#[test]
fn coding_system_type_raw_text() {
    let m = mgr();
    let result = builtin_coding_system_type(&m, vec![Value::symbol("raw-text")]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "raw-text"));
}

#[test]
fn coding_system_type_unknown() {
    let m = mgr();
    let result = builtin_coding_system_type(&m, vec![Value::symbol("bogus")]);
    assert!(result.is_err());
}

// ----- coding-system-change-eol-conversion -----

#[test]
fn change_eol_by_int() {
    let m = mgr();
    let result = builtin_coding_system_change_eol_conversion(
        &m,
        vec![Value::symbol("utf-8"), Value::Int(1)],
    )
    .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-dos"));
}

#[test]
fn change_eol_by_symbol() {
    let m = mgr();
    let result = builtin_coding_system_change_eol_conversion(
        &m,
        vec![Value::symbol("utf-8-unix"), Value::symbol("mac")],
    )
    .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-mac"));
}

#[test]
fn change_eol_strips_existing_suffix() {
    let m = mgr();
    let result = builtin_coding_system_change_eol_conversion(
        &m,
        vec![Value::symbol("utf-8-dos"), Value::Int(0)],
    )
    .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
}

// ----- coding-system-change-text-conversion -----

#[test]
fn change_text_conversion_preserves_eol() {
    let m = mgr();
    let result = builtin_coding_system_change_text_conversion(
        &m,
        vec![Value::symbol("utf-8-unix"), Value::symbol("latin-1")],
    )
    .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
}

#[test]
fn change_text_conversion_undecided_eol() {
    let m = mgr();
    let result = builtin_coding_system_change_text_conversion(
        &m,
        vec![Value::symbol("utf-8"), Value::symbol("latin-1")],
    )
    .unwrap();
    // utf-8 has undecided eol -> no suffix
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "latin-1"));
}

// ----- detect-coding-string -----

#[test]
fn detect_coding_string_highest() {
    let m = mgr();
    let result =
        builtin_detect_coding_string(&m, vec![Value::string("hello"), Value::True]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "undecided"));
}

#[test]
fn detect_coding_string_list() {
    let m = mgr();
    let result = builtin_detect_coding_string(&m, vec![Value::string("hello")]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "undecided"));
}

#[test]
fn detect_coding_string_wrong_type() {
    let m = mgr();
    let result = builtin_detect_coding_string(&m, vec![Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn detect_coding_string_rejects_too_many_args() {
    let m = mgr();
    let result = builtin_detect_coding_string(&m, vec![Value::string("x"), Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

// ----- detect-coding-region -----

#[test]
fn detect_coding_region_highest() {
    let m = mgr();
    let result =
        builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Int(100), Value::True])
            .unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "undecided"));
}

#[test]
fn detect_coding_region_list() {
    let m = mgr();
    let result = builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Int(100)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "undecided"));
}

#[test]
fn detect_coding_region_rejects_too_many_args() {
    let m = mgr();
    let result = builtin_detect_coding_region(
        &m,
        vec![Value::Int(1), Value::Int(100), Value::Nil, Value::Nil],
    );
    assert!(result.is_err());
}

#[test]
fn detect_coding_region_rejects_non_integer_or_marker_bounds() {
    let m = mgr();
    assert!(builtin_detect_coding_region(&m, vec![Value::string("a"), Value::Int(1)]).is_err());
    assert!(builtin_detect_coding_region(&m, vec![Value::Int(1), Value::string("b")]).is_err());
    assert!(builtin_detect_coding_region(&m, vec![Value::Nil, Value::Int(1)]).is_err());
    assert!(builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Nil]).is_err());
}

// ----- keyboard/terminal coding system -----

#[test]
fn keyboard_coding_system_default() {
    let m = mgr();
    let result = builtin_keyboard_coding_system(&m, vec![]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
}

#[test]
fn terminal_coding_system_default() {
    let m = mgr();
    let result = builtin_terminal_coding_system(&m, vec![]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
}

#[test]
fn coding_system_getters_validate_max_arity() {
    let m = mgr();
    assert!(builtin_keyboard_coding_system(&m, vec![Value::Nil]).is_ok());
    assert!(builtin_terminal_coding_system(&m, vec![Value::Nil]).is_ok());
    assert!(builtin_keyboard_coding_system(&m, vec![Value::Nil, Value::Nil]).is_err());
    assert!(builtin_terminal_coding_system(&m, vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn set_keyboard_coding_system() {
    let mut m = mgr();
    let set = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1")]).unwrap();
    assert!(matches!(set, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
    let get = builtin_keyboard_coding_system(&m, vec![]).unwrap();
    assert!(matches!(get, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
}

#[test]
fn set_keyboard_coding_system_canonicalizes_non_unix_alias_suffixes() {
    let mut m = mgr();

    let latin_dos =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-dos")]).unwrap();
    assert_eq!(latin_dos, Value::symbol("iso-latin-1-unix"));

    let latin_mac =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-mac")]).unwrap();
    assert_eq!(latin_mac, Value::symbol("iso-latin-1-unix"));

    let iso_dos =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("iso-8859-1-dos")]).unwrap();
    assert_eq!(iso_dos, Value::symbol("iso-latin-1-unix"));

    let ascii_dos =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-dos")]).unwrap();
    assert_eq!(ascii_dos, Value::symbol("us-ascii-unix"));

    let ascii_mac =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-mac")]).unwrap();
    assert_eq!(ascii_mac, Value::symbol("us-ascii-unix"));
}

#[test]
fn set_keyboard_coding_system_preserves_explicit_unix_spelling() {
    let mut m = mgr();

    let latin_unix =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-unix")]).unwrap();
    assert_eq!(latin_unix, Value::symbol("latin-1-unix"));

    let iso_unix =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("iso-8859-1-unix")]).unwrap();
    assert_eq!(iso_unix, Value::symbol("iso-8859-1-unix"));

    let ascii_unix =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-unix")]).unwrap();
    assert_eq!(ascii_unix, Value::symbol("ascii-unix"));
}

#[test]
fn coding_system_change_eol_conversion_canonicalizes_alias_families() {
    let m = mgr();

    assert_eq!(
        builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("latin-1"), Value::Int(0)],
        )
        .unwrap(),
        Value::symbol("iso-latin-1-unix")
    );
    assert_eq!(
        builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("latin-1-unix"), Value::Nil],
        )
        .unwrap(),
        Value::symbol("iso-latin-1")
    );
    assert_eq!(
        builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("latin-1-unix"), Value::Int(1)],
        )
        .unwrap(),
        Value::symbol("iso-latin-1-dos")
    );
}

#[test]
fn coding_system_change_eol_conversion_canonicalizes_latin9_alias_family() {
    let m = mgr_with_latin9();

    assert_eq!(
        builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("iso-8859-15"), Value::Int(0)],
        )
        .unwrap(),
        Value::symbol("iso-latin-9-unix")
    );
    assert_eq!(
        builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("iso-8859-15-unix"), Value::Nil],
        )
        .unwrap(),
        Value::symbol("iso-latin-9")
    );
    assert_eq!(
        builtin_coding_system_base(&m, vec![Value::symbol("iso-8859-15-unix")]).unwrap(),
        Value::symbol("iso-latin-9")
    );
}

#[test]
fn set_keyboard_coding_system_normalizes_latin9_alias_family() {
    let mut m = mgr_with_latin9();

    let set =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("iso-8859-15")]).unwrap();
    assert_eq!(set, Value::symbol("iso-latin-9-unix"));

    let get = builtin_keyboard_coding_system(&m, vec![]).unwrap();
    assert_eq!(get, Value::symbol("iso-latin-9-unix"));
}

#[test]
fn set_keyboard_coding_system_accepts_alias_derived_variants() {
    let mut m = mgr();

    let latin_unix =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-unix")]).unwrap();
    assert_eq!(latin_unix, Value::symbol("latin-1-unix"));

    let latin_dos =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-dos")]).unwrap();
    assert_eq!(latin_dos, Value::symbol("iso-latin-1-unix"));
}

#[test]
fn set_terminal_coding_system_accepts_alias_derived_variants() {
    let mut m = mgr();

    assert!(
        builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("latin-1-unix")]).is_ok()
    );
    assert_eq!(
        builtin_terminal_coding_system(&m, vec![]).unwrap(),
        Value::symbol("latin-1-unix")
    );
}

#[test]
fn set_terminal_coding_system() {
    let mut m = mgr();
    let set = builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("ascii")]).unwrap();
    assert!(set.is_nil());
    let get = builtin_terminal_coding_system(&m, vec![]).unwrap();
    assert!(matches!(get, Value::Symbol(id) if resolve_sym(id) == "ascii"));
}

#[test]
fn set_keyboard_coding_nil_resets_to_no_conversion() {
    let mut m = mgr();
    builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1")]).unwrap();
    builtin_set_keyboard_coding_system(&mut m, vec![Value::Nil]).unwrap();
    let result = builtin_keyboard_coding_system(&m, vec![]).unwrap();
    assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "no-conversion"));
}

#[test]
fn set_terminal_coding_nil_sets_nil_symbol() {
    let mut m = mgr();
    builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("utf-8")]).unwrap();
    builtin_set_terminal_coding_system(&mut m, vec![Value::Nil]).unwrap();
    let result = builtin_terminal_coding_system(&m, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn coding_system_setters_validate_symbol_and_known_names() {
    let mut m = mgr();
    assert!(builtin_set_keyboard_coding_system(&mut m, vec![Value::string("utf-8")]).is_err());
    assert!(builtin_set_terminal_coding_system(&mut m, vec![Value::string("utf-8")]).is_err());
    assert!(
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("no-such-coding")]).is_err()
    );
    assert!(
        builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("no-such-coding")]).is_err()
    );
}

#[test]
fn coding_system_setters_treat_keywords_as_symbol_designators() {
    let mut m = mgr();
    let keyword = Value::keyword(":utf-8");
    let kb = builtin_set_keyboard_coding_system(&mut m, vec![keyword]);
    let term = builtin_set_terminal_coding_system(&mut m, vec![keyword]);

    match kb {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
        other => panic!("expected coding-system-error for keyword keyboard set, got {other:?}"),
    }
    match term {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
        other => panic!("expected coding-system-error for keyword terminal set, got {other:?}"),
    }
}

#[test]
fn coding_system_setters_validate_arity_edges() {
    let mut m = mgr();
    assert!(builtin_set_keyboard_coding_system(&mut m, vec![Value::Nil, Value::Nil]).is_ok());
    assert!(
        builtin_set_keyboard_coding_system(&mut m, vec![Value::Nil, Value::Nil, Value::Nil])
            .is_err()
    );

    assert!(builtin_set_terminal_coding_system(&mut m, vec![Value::Nil, Value::Nil]).is_ok());
    assert!(
        builtin_set_terminal_coding_system(&mut m, vec![Value::Nil, Value::Nil, Value::Nil])
            .is_ok()
    );
    assert!(
        builtin_set_terminal_coding_system(
            &mut m,
            vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]
        )
        .is_err()
    );
}

// ----- coding-system-priority-list -----

#[test]
fn priority_list_full() {
    let m = mgr();
    let result = builtin_coding_system_priority_list(&m, vec![]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert!(!items.is_empty());
    // First should be utf-8
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
}

#[test]
fn priority_list_highest() {
    let m = mgr();
    let result = builtin_coding_system_priority_list(&m, vec![Value::True]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
}

#[test]
fn priority_list_rejects_too_many_args() {
    let m = mgr();
    let result = builtin_coding_system_priority_list(&m, vec![Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

// ----- EolType -----

#[test]
fn eol_type_to_int() {
    assert_eq!(EolType::Unix.to_int(), 0);
    assert_eq!(EolType::Dos.to_int(), 1);
    assert_eq!(EolType::Mac.to_int(), 2);
    assert_eq!(EolType::Undecided.to_int(), 0);
}

#[test]
fn eol_type_from_suffix() {
    assert_eq!(EolType::from_suffix("utf-8-unix"), Some(EolType::Unix));
    assert_eq!(EolType::from_suffix("utf-8-dos"), Some(EolType::Dos));
    assert_eq!(EolType::from_suffix("utf-8-mac"), Some(EolType::Mac));
    assert_eq!(EolType::from_suffix("utf-8"), None);
}

// ----- strip_eol_suffix -----

#[test]
fn strip_eol_suffix_works() {
    assert_eq!(strip_eol_suffix("utf-8-unix"), "utf-8");
    assert_eq!(strip_eol_suffix("utf-8-dos"), "utf-8");
    assert_eq!(strip_eol_suffix("utf-8-mac"), "utf-8");
    assert_eq!(strip_eol_suffix("utf-8"), "utf-8");
    assert_eq!(strip_eol_suffix("latin-1"), "latin-1");
}

// ----- argument validation -----

#[test]
fn coding_system_get_wrong_arg_count() {
    let m = mgr();
    let result = builtin_coding_system_get(&m, vec![Value::symbol("utf-8")]);
    assert!(result.is_err());
}

#[test]
fn coding_system_base_wrong_arg_count() {
    let m = mgr();
    let result = builtin_coding_system_base(&m, vec![]);
    assert!(result.is_err());
}

#[test]
fn coding_system_aliases_wrong_arg_count() {
    let m = mgr();
    let result = builtin_coding_system_aliases(&m, vec![]);
    assert!(result.is_err());
}

#[test]
fn coding_system_p_reads_runtime_aliases() {
    let mut m = mgr();
    let before = builtin_coding_system_p(&m, vec![Value::symbol("vm-utf8")]).unwrap();
    assert!(before.is_nil());

    builtin_define_coding_system_alias(
        &mut m,
        vec![Value::symbol("vm-utf8"), Value::symbol("utf-8")],
    )
    .unwrap();
    let after = builtin_coding_system_p(&m, vec![Value::symbol("vm-utf8")]).unwrap();
    assert!(after.is_truthy());
}

#[test]
fn coding_system_p_accepts_nil_and_supported_derived_variants() {
    let m = mgr();
    assert!(
        builtin_coding_system_p(&m, vec![Value::Nil])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_coding_system_p(&m, vec![Value::symbol("ascii-dos")])
            .unwrap()
            .is_truthy()
    );
}

#[test]
fn check_coding_system_signals_unknown_symbols() {
    let m = mgr();
    let result = builtin_check_coding_system(&m, vec![Value::symbol("vm-no-such")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "coding-system-error");
            assert_eq!(sig.data, vec![Value::symbol("vm-no-such")]);
        }
        other => panic!("expected coding-system-error signal, got {other:?}"),
    }
}

#[test]
fn check_coding_system_accepts_supported_derived_variants() {
    let m = mgr();
    assert_eq!(
        builtin_check_coding_system(&m, vec![Value::symbol("latin-1-unix")]).unwrap(),
        Value::symbol("latin-1-unix")
    );
    assert_eq!(
        builtin_check_coding_system(&m, vec![Value::symbol("ascii-unix")]).unwrap(),
        Value::symbol("ascii-unix")
    );
    assert_eq!(
        builtin_check_coding_system(&m, vec![Value::symbol("undecided-unix")]).unwrap(),
        Value::symbol("undecided-unix")
    );
    assert_eq!(
        builtin_check_coding_system(&m, vec![Value::symbol("utf-8-auto-unix")]).unwrap(),
        Value::symbol("utf-8-auto-unix")
    );
    assert_eq!(
        builtin_check_coding_system(&m, vec![Value::symbol("prefer-utf-8-unix")]).unwrap(),
        Value::symbol("prefer-utf-8-unix")
    );
}

#[test]
fn check_coding_system_rejects_unsupported_derived_variants() {
    let m = mgr();
    assert!(builtin_check_coding_system(&m, vec![Value::symbol("no-conversion-unix")]).is_err());
    assert!(builtin_check_coding_system(&m, vec![Value::symbol("binary-unix")]).is_err());
    assert!(builtin_check_coding_system(&m, vec![Value::symbol("emacs-internal-unix")]).is_err());
}

#[test]
fn check_coding_systems_region_semantics() {
    let m = mgr();
    assert!(
        builtin_check_coding_systems_region(
            &m,
            vec![
                Value::Int(1),
                Value::Int(1),
                Value::list(vec![Value::symbol("utf-8")])
            ]
        )
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_check_coding_systems_region(
            &m,
            vec![Value::string("x"), Value::Int(1), Value::symbol("utf-8")]
        )
        .unwrap()
        .is_nil()
    );

    let type_err = builtin_check_coding_systems_region(
        &m,
        vec![Value::Int(1), Value::string("x"), Value::symbol("utf-8")],
    )
    .unwrap_err();
    match type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::string("x")]
            );
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }

    assert!(builtin_check_coding_systems_region(&m, vec![]).is_err());
    assert!(builtin_check_coding_systems_region(&m, vec![Value::Int(1), Value::Int(1)]).is_err());
}

#[test]
fn set_keyboard_coding_system_rejects_unsuitable_variants() {
    let mut m = mgr();
    let auto = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("utf-8-auto")]);
    let auto_derived =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("utf-8-auto-unix")]);
    let prefer = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("prefer-utf-8")]);
    let prefer_derived =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("prefer-utf-8-unix")]);
    let undecided = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("undecided")]);
    let undecided_derived =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("undecided-unix")]);

    assert!(auto.is_err());
    assert!(auto_derived.is_err());
    assert!(prefer.is_err());
    assert!(prefer_derived.is_err());
    assert!(undecided.is_err());
    assert!(undecided_derived.is_err());
}

#[test]
fn set_keyboard_coding_system_preserves_emacs_internal() {
    let mut m = mgr();
    let set =
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("emacs-internal")]).unwrap();
    assert_eq!(set, Value::symbol("emacs-internal"));

    let get = builtin_keyboard_coding_system(&m, vec![]).unwrap();
    assert_eq!(get, Value::symbol("emacs-internal"));
}

#[test]
fn find_coding_system_known_and_unknown() {
    let m = mgr();
    let known = builtin_find_coding_system(&m, vec![Value::symbol("utf-8")]).unwrap();
    assert_eq!(known, Value::symbol("utf-8"));

    let unknown = builtin_find_coding_system(&m, vec![Value::symbol("vm-no-such-coding")]).unwrap();
    assert_eq!(unknown, Value::Nil);
}

#[test]
fn set_coding_system_priority_reorders_front_in_arg_order() {
    let mut m = mgr();
    builtin_set_coding_system_priority(
        &mut m,
        vec![Value::symbol("raw-text"), Value::symbol("utf-8")],
    )
    .unwrap();

    let list = builtin_coding_system_priority_list(&m, vec![]).unwrap();
    let items = list_to_vec(&list).unwrap();
    assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "raw-text"));
    assert!(matches!(&items[1], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
}

#[test]
fn set_coding_system_priority_rejects_nil_payload() {
    let mut m = mgr();
    let result = builtin_set_coding_system_priority(&mut m, vec![Value::Nil]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("coding-system-p"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn set_coding_system_priority_keyword_signals_coding_system_error() {
    let mut m = mgr();
    let result = builtin_set_coding_system_priority(&mut m, vec![Value::keyword(":utf-8")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
        other => panic!("expected coding-system-error signal, got {other:?}"),
    }
}

#[test]
fn set_coding_system_priority_string_is_type_error() {
    let mut m = mgr();
    let result = builtin_set_coding_system_priority(&mut m, vec![Value::string("utf-8")]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
}

#[test]
fn internal_coding_system_setters_match_surface_validation() {
    let mut m = mgr();
    assert_eq!(
        builtin_set_keyboard_coding_system_internal(&mut m, vec![Value::symbol("utf-8")]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_set_terminal_coding_system_internal(&mut m, vec![Value::symbol("utf-8")]).unwrap(),
        Value::Nil
    );
    assert_eq!(
        builtin_set_safe_terminal_coding_system_internal(&mut m, vec![Value::symbol("utf-8")])
            .unwrap(),
        Value::Nil
    );
    assert!(
        builtin_set_keyboard_coding_system_internal(&mut m, vec![Value::symbol("foo")]).is_err()
    );
    assert!(
        builtin_set_terminal_coding_system_internal(&mut m, vec![Value::symbol("foo")]).is_err()
    );
    assert!(
        builtin_set_safe_terminal_coding_system_internal(&mut m, vec![Value::symbol("foo")])
            .is_err()
    );
}

#[test]
fn text_quoting_and_conversion_style_basics() {
    assert_eq!(
        builtin_text_quoting_style(vec![]).expect("text-quoting-style"),
        Value::symbol("curve")
    );
    assert!(builtin_text_quoting_style(vec![Value::Nil]).is_err());
    assert_eq!(
        builtin_set_text_conversion_style(vec![Value::symbol("latin-1")])
            .expect("set-text-conversion-style"),
        Value::Nil
    );
    assert_eq!(
        builtin_set_text_conversion_style(vec![Value::symbol("foo"), Value::symbol("bar")])
            .expect("set-text-conversion-style 2 args"),
        Value::Nil
    );
    assert!(builtin_set_text_conversion_style(vec![]).is_err());
}

#[test]
fn text_quoting_style_variable_defaults_to_nil() {
    let eval = crate::emacs_core::eval::Evaluator::new();
    assert_eq!(
        eval.obarray.symbol_value("text-quoting-style"),
        Some(&Value::Nil)
    );
}
