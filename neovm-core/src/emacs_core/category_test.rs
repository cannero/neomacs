use super::*;

fn reset_pure_category_manager_for_tests() {
    PURE_CATEGORY_MANAGER.with(|slot| {
        *slot.borrow_mut() = CategoryManager::new();
    });
}

// -----------------------------------------------------------------------
// CategoryTable
// -----------------------------------------------------------------------

#[test]
fn new_table_is_empty() {
    let table = CategoryTable::new();
    assert!(table.entries.is_empty());
    assert!(table.descriptions.contains_key(&'1'));
}

#[test]
fn define_category_stores_description() {
    let mut table = CategoryTable::new();
    table.define_category('a', "ASCII letters").unwrap();
    assert_eq!(table.category_docstring('a'), Some("ASCII letters"));
}

#[test]
fn define_category_rejects_control_chars() {
    let mut table = CategoryTable::new();
    // '1' is valid (0x31), ' ' is valid (0x20 per official Emacs).
    assert!(table.define_category('1', "digits").is_ok());
    assert!(table.define_category(' ', "space").is_ok());
    // Control chars and non-ASCII are rejected.
    assert!(table.define_category('\n', "newline").is_err());
    assert!(table.define_category('\x7F', "DEL").is_err());
}

#[test]
fn define_category_accepts_upper_and_lower() {
    let mut table = CategoryTable::new();
    table.define_category('a', "lower a").unwrap();
    table.define_category('Z', "upper Z").unwrap();
    table.define_category('!', "bang").unwrap();
    assert_eq!(table.category_docstring('a'), Some("lower a"));
    assert_eq!(table.category_docstring('Z'), Some("upper Z"));
    assert_eq!(table.category_docstring('!'), Some("bang"));
}

#[test]
fn category_docstring_returns_none_for_undefined() {
    let table = CategoryTable::new();
    assert_eq!(table.category_docstring('x'), None);
}

#[test]
fn get_unused_category_empty_table() {
    let table = CategoryTable::new();
    // First unused is 'a'.
    assert_eq!(table.get_unused_category(), Some('a'));
}

#[test]
fn get_unused_category_skips_defined() {
    let mut table = CategoryTable::new();
    table.define_category('a', "used").unwrap();
    assert_eq!(table.get_unused_category(), Some('b'));
}

#[test]
fn get_unused_category_all_lower_used() {
    let mut table = CategoryTable::new();
    for ch in 'a'..='z' {
        table.define_category(ch, "used").unwrap();
    }
    // Should return 'A' (first uppercase).
    assert_eq!(table.get_unused_category(), Some('A'));
}

#[test]
fn get_unused_category_all_used() {
    let mut table = CategoryTable::new();
    for ch in 'a'..='z' {
        table.define_category(ch, "used").unwrap();
    }
    for ch in 'A'..='Z' {
        table.define_category(ch, "used").unwrap();
    }
    assert_eq!(table.get_unused_category(), None);
}

#[test]
fn modify_entry_adds_category() {
    let mut table = CategoryTable::new();
    table.modify_entry('X', 'a', false).unwrap();
    let cats = table.char_category_set('X');
    assert!(cats.contains(&'a'));
}

#[test]
fn modify_entry_removes_category_with_reset() {
    let mut table = CategoryTable::new();
    table.modify_entry('X', 'a', false).unwrap();
    table.modify_entry('X', 'b', false).unwrap();
    table.modify_entry('X', 'a', true).unwrap();
    let cats = table.char_category_set('X');
    assert!(!cats.contains(&'a'));
    assert!(cats.contains(&'b'));
}

#[test]
fn modify_entry_rejects_control_chars() {
    let mut table = CategoryTable::new();
    // Space (0x20) is valid per official Emacs CATEGORYP.
    table.define_category(' ', "space").unwrap();
    assert!(table.modify_entry('X', ' ', false).is_ok());
    // Control chars are rejected.
    assert!(table.modify_entry('X', '\n', false).is_err());
}

#[test]
fn char_category_set_empty_for_unknown() {
    let table = CategoryTable::new();
    let cats = table.char_category_set('Z');
    assert!(cats.is_empty());
}

#[test]
fn char_category_set_multiple_categories() {
    let mut table = CategoryTable::new();
    table.modify_entry('!', 'a', false).unwrap();
    table.modify_entry('!', 'b', false).unwrap();
    table.modify_entry('!', 'c', false).unwrap();
    let cats = table.char_category_set('!');
    assert_eq!(cats.len(), 3);
    assert!(cats.contains(&'a'));
    assert!(cats.contains(&'b'));
    assert!(cats.contains(&'c'));
}

// -----------------------------------------------------------------------
// CategoryManager
// -----------------------------------------------------------------------

#[test]
fn manager_new_has_standard_table() {
    let mgr = CategoryManager::new();
    assert_eq!(mgr.current_table, "standard");
    assert!(mgr.tables.contains_key("standard"));
}

#[test]
fn manager_current_returns_standard() {
    let mgr = CategoryManager::new();
    // Should not panic.
    let _table = mgr.current();
}

#[test]
fn manager_current_mut_allows_modification() {
    let mut mgr = CategoryManager::new();
    mgr.current_mut().define_category('a', "test").unwrap();
    assert_eq!(mgr.current().category_docstring('a'), Some("test"));
}

#[test]
fn manager_standard_and_current_are_same_initially() {
    let mut mgr = CategoryManager::new();
    mgr.standard_mut().define_category('z', "zed").unwrap();
    // Since current == standard, current should see it too.
    assert_eq!(mgr.current().category_docstring('z'), Some("zed"));
}

// -----------------------------------------------------------------------
// Pure builtins
// -----------------------------------------------------------------------

#[test]
fn builtin_define_category_basic() {
    reset_pure_category_manager_for_tests();
    let result = builtin_define_category_inner(vec![Value::Char('a'), Value::string("ASCII letters")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn builtin_define_category_wrong_args() {
    // Too few.
    assert!(builtin_define_category_inner(vec![Value::Char('a')]).is_err());
    // Too many.
    assert!(
        builtin_define_category_inner(vec![
            Value::Char('a'),
            Value::string("doc"),
            Value::Nil,
            Value::Nil,
        ])
        .is_err()
    );
}

#[test]
fn builtin_define_category_invalid_char() {
    // Control chars are rejected; space (0x20) is valid per official Emacs.
    let result = builtin_define_category_inner(vec![Value::Char('\n'), Value::string("newline")]);
    assert!(result.is_err());
}

#[test]
fn builtin_define_category_wrong_type_docstring() {
    let result = builtin_define_category_inner(vec![Value::Char('a'), Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn builtin_category_docstring_basic() {
    reset_pure_category_manager_for_tests();
    builtin_define_category_inner(vec![Value::Char('a'), Value::string("ASCII letters")]).unwrap();
    let result = builtin_category_docstring_inner(vec![Value::Char('a')]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("ASCII letters"));
}

#[test]
fn builtin_category_docstring_wrong_args() {
    assert!(builtin_category_docstring_inner(vec![]).is_err());
    assert!(builtin_category_docstring_inner(vec![Value::Char('a'), Value::Nil, Value::Nil,]).is_err());
}

#[test]
fn builtin_get_unused_category_returns_char() {
    reset_pure_category_manager_for_tests();
    let result = builtin_get_unused_category_inner(vec![]).unwrap();
    assert!(matches!(result, Value::Char(_)));
}

#[test]
fn builtin_get_unused_category_wrong_args() {
    assert!(builtin_get_unused_category_inner(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_category_table_p_nil_for_t() {
    let result = builtin_category_table_p(vec![Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn builtin_category_table_p_true_for_category_char_table() {
    let table = builtin_make_category_table(vec![]).unwrap();
    let result = builtin_category_table_p(vec![table]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn builtin_category_table_p_false_for_int() {
    let result = builtin_category_table_p(vec![Value::Int(5)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn builtin_category_table_p_wrong_args() {
    assert!(builtin_category_table_p(vec![]).is_err());
    assert!(builtin_category_table_p(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_category_table_returns_category_table() {
    let result = builtin_category_table_inner(vec![]).unwrap();
    assert!(builtin_category_table_p(vec![result]).unwrap().is_truthy());
}

#[test]
fn builtin_category_table_wrong_args() {
    assert!(builtin_category_table_inner(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_standard_category_table_returns_category_table() {
    let result = builtin_standard_category_table_inner(vec![]).unwrap();
    assert!(builtin_category_table_p(vec![result]).unwrap().is_truthy());
}

#[test]
fn builtin_standard_category_table_wrong_args() {
    assert!(builtin_standard_category_table_inner(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_make_category_table_returns_category_table() {
    let result = builtin_make_category_table(vec![]).unwrap();
    assert!(builtin_category_table_p(vec![result]).unwrap().is_truthy());
}

#[test]
fn builtin_make_category_table_wrong_args() {
    assert!(builtin_make_category_table(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_set_category_table_returns_arg() {
    let table = builtin_make_category_table(vec![]).unwrap();
    let result = builtin_set_category_table_inner(vec![table]).unwrap();
    assert!(equal_value(&result, &table, 0));
}

#[test]
fn builtin_set_category_table_nil_returns_standard() {
    let result = builtin_set_category_table_inner(vec![Value::Nil]).unwrap();
    let standard = builtin_standard_category_table_inner(vec![]).unwrap();
    assert!(equal_value(&result, &standard, 0));
}

#[test]
fn builtin_set_category_table_wrong_args() {
    assert!(builtin_set_category_table_inner(vec![]).is_err());
    assert!(builtin_set_category_table_inner(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_make_category_set_basic() {
    let result = builtin_make_category_set(vec![Value::string("abc")]).unwrap();
    // Should be a bool-vector of length 128.
    if let Value::Vector(arc) = &result {
        let vec = with_heap(|h| h.get_vector(*arc).clone());
        // Tag + size + 128 bits = 130 elements.
        assert_eq!(vec.len(), 130);
        assert!(matches!(&vec[0], Value::Symbol(id) if resolve_sym(*id) == "--bool-vector--"));
        assert!(matches!(&vec[1], Value::Int(128)));
        // 'a' = 97, 'b' = 98, 'c' = 99 should be set.
        assert!(matches!(&vec[2 + 97], Value::Int(1)));
        assert!(matches!(&vec[2 + 98], Value::Int(1)));
        assert!(matches!(&vec[2 + 99], Value::Int(1)));
        // 'd' = 100 should NOT be set.
        assert!(matches!(&vec[2 + 100], Value::Int(0)));
    } else {
        panic!("Expected vector result");
    }
}

#[test]
fn builtin_make_category_set_empty_string() {
    let result = builtin_make_category_set(vec![Value::string("")]).unwrap();
    if let Value::Vector(arc) = &result {
        let vec = with_heap(|h| h.get_vector(*arc).clone());
        assert_eq!(vec.len(), 130);
        // All bits should be 0.
        for i in 2..130 {
            assert!(matches!(&vec[i], Value::Int(0)));
        }
    } else {
        panic!("Expected vector result");
    }
}

#[test]
fn builtin_make_category_set_uppercase() {
    let result = builtin_make_category_set(vec![Value::string("AZ")]).unwrap();
    if let Value::Vector(arc) = &result {
        let vec = with_heap(|h| h.get_vector(*arc).clone());
        // 'A' = 65, 'Z' = 90
        assert!(matches!(&vec[2 + 65], Value::Int(1)));
        assert!(matches!(&vec[2 + 90], Value::Int(1)));
        assert!(matches!(&vec[2 + 66], Value::Int(0)));
    } else {
        panic!("Expected vector result");
    }
}

#[test]
fn builtin_make_category_set_includes_ascii_graphic_symbols() {
    let result = builtin_make_category_set(vec![Value::string("a1b!c")]).unwrap();
    if let Value::Vector(arc) = &result {
        let vec = with_heap(|h| h.get_vector(*arc).clone());
        // 'a', '1', 'b', '!', 'c' all set.
        assert!(matches!(&vec[2 + 97], Value::Int(1))); // 'a'
        assert!(matches!(&vec[2 + 98], Value::Int(1))); // 'b'
        assert!(matches!(&vec[2 + 99], Value::Int(1))); // 'c'
        assert!(matches!(&vec[2 + 49], Value::Int(1))); // '1'
        assert!(matches!(&vec[2 + 33], Value::Int(1))); // '!'
    } else {
        panic!("Expected vector result");
    }
}

#[test]
fn builtin_make_category_set_wrong_type() {
    assert!(builtin_make_category_set(vec![Value::Int(42)]).is_err());
}

#[test]
fn builtin_make_category_set_wrong_args() {
    assert!(builtin_make_category_set(vec![]).is_err());
    assert!(builtin_make_category_set(vec![Value::string("a"), Value::string("b"),]).is_err());
}

#[test]
fn builtin_category_set_mnemonics_round_trip() {
    let set = builtin_make_category_set(vec![Value::string("a1!")]).unwrap();
    assert_eq!(
        builtin_category_set_mnemonics(vec![set]).unwrap(),
        Value::string("!1a")
    );
}

#[test]
fn builtin_category_set_mnemonics_rejects_non_category_set() {
    let nil_err = builtin_category_set_mnemonics(vec![Value::Nil]).unwrap_err();
    match nil_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("categorysetp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }

    let non_category_set = Value::vector(vec![
        Value::symbol("--bool-vector--"),
        Value::Int(2),
        Value::Int(1),
        Value::Int(0),
    ]);
    let short_err = builtin_category_set_mnemonics(vec![non_category_set]).unwrap_err();
    match short_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("categorysetp"), non_category_set]
            );
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }
}

#[test]
fn builtin_category_set_mnemonics_wrong_args() {
    assert!(builtin_category_set_mnemonics(vec![]).is_err());
    assert!(builtin_category_set_mnemonics(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_define_category_eval_sets_docstring() {
    let mut eval = super::super::eval::Context::new();
    let result = builtin_define_category(
        &mut eval,
        vec![Value::Char('Z'), Value::string("neovm-category-doc")],
    )
    .unwrap();
    assert!(result.is_nil());

    let doc = builtin_category_docstring(&mut eval, vec![Value::Char('Z')]).unwrap();
    assert_eq!(doc.as_str(), Some("neovm-category-doc"));
}

#[test]
fn builtin_get_unused_category_eval_tracks_defined_values() {
    let mut eval = super::super::eval::Context::new();
    let first = builtin_get_unused_category(&mut eval, vec![]).unwrap();
    assert!(matches!(first, Value::Char('a')));

    builtin_define_category(&mut eval, vec![Value::Char('a'), Value::string("used")]).unwrap();
    let second = builtin_get_unused_category(&mut eval, vec![]).unwrap();
    assert!(matches!(second, Value::Char('b')));
}

#[test]
fn builtin_category_table_eval_defaults_to_standard() {
    let mut eval = super::super::eval::Context::new();
    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    let standard = builtin_standard_category_table(&mut eval, vec![]).unwrap();
    assert!(equal_value(&current, &standard, 0));
}

#[test]
fn builtin_set_category_table_eval_roundtrip() {
    let mut eval = super::super::eval::Context::new();
    let custom = builtin_make_category_table(vec![]).unwrap();

    let out = builtin_set_category_table(&mut eval, vec![custom]).unwrap();
    assert!(equal_value(&out, &custom, 0));

    let current = builtin_category_table(&mut eval, vec![]).unwrap();
    assert!(equal_value(&current, &custom, 0));
}

#[test]
fn builtin_set_category_table_eval_nil_after_custom_clones_standard() {
    let mut eval = super::super::eval::Context::new();
    let standard = builtin_standard_category_table(&mut eval, vec![]).unwrap();
    let custom = builtin_make_category_table(vec![]).unwrap();

    builtin_set_category_table(&mut eval, vec![custom]).unwrap();
    let restored = builtin_set_category_table(&mut eval, vec![Value::Nil]).unwrap();

    assert!(
        builtin_category_table_p(vec![restored])
            .unwrap()
            .is_truthy()
    );
    assert!(!category_table_pointer_eq(&restored, &standard));
}

#[test]
fn builtin_set_category_table_eval_rejects_non_tables() {
    let mut eval = super::super::eval::Context::new();
    let result = builtin_set_category_table(&mut eval, vec![Value::Int(1)]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

#[test]
fn is_category_letter_valid() {
    assert!(is_category_letter('a'));
    assert!(is_category_letter('z'));
    assert!(is_category_letter('A'));
    assert!(is_category_letter('Z'));
    assert!(is_category_letter('m'));
    assert!(is_category_letter('M'));
    assert!(is_category_letter('0'));
    assert!(is_category_letter('9'));
    assert!(is_category_letter('!'));
}

#[test]
fn is_category_letter_space_is_valid() {
    // Official Emacs: CATEGORYP = RANGED_FIXNUMP(0x20, x, 0x7E)
    assert!(is_category_letter(' '));
}

#[test]
fn is_category_letter_invalid() {
    assert!(!is_category_letter('\x1F')); // below 0x20
    assert!(!is_category_letter('\x7F')); // DEL, above 0x7E
    assert!(!is_category_letter('\n'));
    assert!(!is_category_letter('é'));
}

#[test]
fn extract_char_from_char_value() {
    let result = extract_char(&Value::Char('x'), "test");
    assert_eq!(result.unwrap(), 'x');
}

#[test]
fn extract_char_from_int_value() {
    let result = extract_char(&Value::Int(65), "test");
    assert_eq!(result.unwrap(), 'A');
}

#[test]
fn extract_char_wrong_type() {
    let result = extract_char(&Value::string("not a char"), "test");
    assert!(result.is_err());
}
