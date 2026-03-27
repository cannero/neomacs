use super::*;
use crate::emacs_core::intern::intern;

// -----------------------------------------------------------------------
// CaseTable tests
// -----------------------------------------------------------------------

#[test]
fn standard_ascii_upcase() {
    let table = CaseTable::standard_ascii();
    assert_eq!(table.upcase.get(&'a'), Some(&'A'));
    assert_eq!(table.upcase.get(&'z'), Some(&'Z'));
    assert_eq!(table.upcase.get(&'m'), Some(&'M'));
    // Uppercase letters should have no upcase mapping.
    assert_eq!(table.upcase.get(&'A'), None);
}

#[test]
fn standard_ascii_downcase() {
    let table = CaseTable::standard_ascii();
    assert_eq!(table.downcase.get(&'A'), Some(&'a'));
    assert_eq!(table.downcase.get(&'Z'), Some(&'z'));
    assert_eq!(table.downcase.get(&'M'), Some(&'m'));
    // Lowercase letters should have no downcase mapping.
    assert_eq!(table.downcase.get(&'a'), None);
}

#[test]
fn standard_ascii_canonicalize() {
    let table = CaseTable::standard_ascii();
    // Both upper and lower should canonicalize to lowercase.
    assert_eq!(table.canonicalize.get(&'A'), Some(&'a'));
    assert_eq!(table.canonicalize.get(&'a'), Some(&'a'));
    assert_eq!(table.canonicalize.get(&'Z'), Some(&'z'));
    assert_eq!(table.canonicalize.get(&'z'), Some(&'z'));
}

#[test]
fn standard_ascii_equivalences() {
    let table = CaseTable::standard_ascii();
    // Equivalences form a cycle: A -> a -> A.
    assert_eq!(table.equivalences.get(&'A'), Some(&'a'));
    assert_eq!(table.equivalences.get(&'a'), Some(&'A'));
}

#[test]
fn empty_table_has_no_mappings() {
    let table = CaseTable::empty();
    assert!(table.upcase.is_empty());
    assert!(table.downcase.is_empty());
    assert!(table.canonicalize.is_empty());
    assert!(table.equivalences.is_empty());
}

// -----------------------------------------------------------------------
// CaseTableManager tests
// -----------------------------------------------------------------------

#[test]
fn manager_upcase_char() {
    let mgr = CaseTableManager::new();
    assert_eq!(mgr.upcase_char('a'), 'A');
    assert_eq!(mgr.upcase_char('z'), 'Z');
    assert_eq!(mgr.upcase_char('A'), 'A'); // already uppercase, no mapping
    assert_eq!(mgr.upcase_char('0'), '0'); // non-letter unchanged
    assert_eq!(mgr.upcase_char(' '), ' ');
}

#[test]
fn manager_downcase_char() {
    let mgr = CaseTableManager::new();
    assert_eq!(mgr.downcase_char('A'), 'a');
    assert_eq!(mgr.downcase_char('Z'), 'z');
    assert_eq!(mgr.downcase_char('a'), 'a'); // already lowercase, no mapping
    assert_eq!(mgr.downcase_char('5'), '5'); // non-letter unchanged
}

#[test]
fn manager_upcase_string() {
    let mgr = CaseTableManager::new();
    assert_eq!(mgr.upcase_string("hello"), "HELLO");
    assert_eq!(mgr.upcase_string("Hello World"), "HELLO WORLD");
    assert_eq!(mgr.upcase_string("ABC"), "ABC");
    assert_eq!(mgr.upcase_string(""), "");
    assert_eq!(mgr.upcase_string("a1b2c3"), "A1B2C3");
}

#[test]
fn manager_downcase_string() {
    let mgr = CaseTableManager::new();
    assert_eq!(mgr.downcase_string("HELLO"), "hello");
    assert_eq!(mgr.downcase_string("Hello World"), "hello world");
    assert_eq!(mgr.downcase_string("abc"), "abc");
    assert_eq!(mgr.downcase_string(""), "");
    assert_eq!(mgr.downcase_string("A1B2C3"), "a1b2c3");
}

#[test]
fn manager_default() {
    let mgr = CaseTableManager::default();
    assert_eq!(mgr.upcase_char('a'), 'A');
    assert_eq!(mgr.downcase_char('A'), 'a');
}

#[test]
fn manager_set_current() {
    let mut mgr = CaseTableManager::new();
    let mut custom = CaseTable::empty();
    // Map 'x' to 'Y' for upcase.
    custom.upcase.insert('x', 'Y');
    mgr.set_current(custom);
    assert_eq!(mgr.upcase_char('x'), 'Y');
    // 'a' no longer has an upcase mapping in the custom table.
    assert_eq!(mgr.upcase_char('a'), 'a');
}

#[test]
fn manager_set_standard() {
    let mut mgr = CaseTableManager::new();
    let custom = CaseTable::empty();
    mgr.set_standard(custom);
    assert!(mgr.standard_table().upcase.is_empty());
}

// -----------------------------------------------------------------------
// Builtin tests
// -----------------------------------------------------------------------

#[test]
fn builtin_case_table_p_on_non_table() {
    assert!(matches!(
        builtin_case_table_p(vec![Value::Nil]).unwrap(),
        Value::Nil
    ));
    assert!(matches!(
        builtin_case_table_p(vec![Value::Int(42)]).unwrap(),
        Value::Nil
    ));
    assert!(matches!(
        builtin_case_table_p(vec![Value::string("hello")]).unwrap(),
        Value::Nil
    ));
}

#[test]
fn builtin_case_table_p_on_char_table() {
    // A proper char-table with case-table subtype.
    let ct = make_case_table_value();
    assert!(matches!(
        builtin_case_table_p(vec![ct]).unwrap(),
        Value::True
    ));
}

#[test]
fn builtin_case_table_p_wrong_arg_count() {
    assert!(builtin_case_table_p(vec![]).is_err());
    assert!(builtin_case_table_p(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_current_case_table_returns_case_table() {
    let result = builtin_current_case_table_inner(vec![]).unwrap();
    assert!(is_case_table(&result));
}

#[test]
fn builtin_current_case_table_wrong_args() {
    assert!(builtin_current_case_table_inner(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_standard_case_table_returns_case_table() {
    let result = builtin_standard_case_table_inner(vec![]).unwrap();
    assert!(is_case_table(&result));
}

#[test]
fn builtin_standard_case_table_wrong_args() {
    assert!(builtin_standard_case_table_inner(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_set_case_table_returns_arg() {
    let table = make_case_table_value();
    let result = builtin_set_case_table_inner(vec![table]).unwrap();
    assert_eq!(result, table);
}

#[test]
fn builtin_set_case_table_rejects_non_table() {
    assert!(builtin_set_case_table_inner(vec![Value::Int(1)]).is_err());
}

#[test]
fn builtin_set_case_table_wrong_args() {
    assert!(builtin_set_case_table_inner(vec![]).is_err());
    assert!(builtin_set_case_table_inner(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn builtin_set_standard_case_table_returns_arg() {
    let mut ctx = super::super::eval::Context::new();
    let table = make_case_table_value();
    let result = builtin_set_standard_case_table(&mut ctx, vec![table]).unwrap();
    assert_eq!(result, table);
}

#[test]
fn builtin_set_standard_case_table_rejects_non_table() {
    let mut ctx = super::super::eval::Context::new();
    assert!(builtin_set_standard_case_table(&mut ctx, vec![Value::Int(1)]).is_err());
}

#[test]
fn builtin_set_standard_case_table_wrong_args() {
    let mut ctx = super::super::eval::Context::new();
    assert!(builtin_set_standard_case_table(&mut ctx, vec![]).is_err());
}

#[test]
fn evaluator_case_table_roundtrip_and_isolation() {
    let mut eval = super::super::eval::Context::new();
    let standard = builtin_standard_case_table(&mut eval, vec![]).unwrap();
    let current = builtin_current_case_table(&mut eval, vec![]).unwrap();
    assert_eq!(standard, current);

    let current_id = eval.buffers.current_buffer().expect("current buffer").id;
    let other_id = eval.buffers.create_buffer("*case-other*");

    let custom = make_case_table_value();
    builtin_set_case_table(&mut eval, vec![custom]).unwrap();
    let after_set = builtin_current_case_table(&mut eval, vec![]).unwrap();
    assert_eq!(after_set, custom);

    eval.buffers.set_current(other_id);
    let other_current = builtin_current_case_table(&mut eval, vec![]).unwrap();
    assert_eq!(other_current, standard);

    eval.buffers.set_current(current_id);
    let restored = builtin_current_case_table(&mut eval, vec![]).unwrap();
    assert_eq!(restored, custom);
}

#[test]
fn builtin_downcase_char_uppercase() {
    // (downcase ?A) -> 97 (i.e., ?a)
    let result = builtin_downcase_char(vec![Value::Char('A')]).unwrap();
    assert!(matches!(result, Value::Int(97)));
}

#[test]
fn builtin_downcase_char_lowercase_unchanged() {
    // (downcase ?a) -> 97
    let result = builtin_downcase_char(vec![Value::Char('a')]).unwrap();
    assert!(matches!(result, Value::Int(97)));
}

#[test]
fn builtin_downcase_char_from_int() {
    // (downcase 65) -> 97 (65 = ?A, 97 = ?a)
    let result = builtin_downcase_char(vec![Value::Int(65)]).unwrap();
    assert!(matches!(result, Value::Int(97)));
}

#[test]
fn builtin_downcase_char_wrong_type() {
    assert!(builtin_downcase_char(vec![Value::string("A")]).is_err());
    assert!(builtin_downcase_char(vec![Value::Nil]).is_err());
}

#[test]
fn builtin_downcase_char_wrong_arg_count() {
    assert!(builtin_downcase_char(vec![]).is_err());
    assert!(builtin_downcase_char(vec![Value::Char('A'), Value::Char('B')]).is_err());
}

#[test]
fn upcase_all_letters() {
    let mgr = CaseTableManager::new();
    for lower in b'a'..=b'z' {
        let lc = lower as char;
        let uc = (lower - b'a' + b'A') as char;
        assert_eq!(mgr.upcase_char(lc), uc);
    }
}

#[test]
fn downcase_all_letters() {
    let mgr = CaseTableManager::new();
    for upper in b'A'..=b'Z' {
        let uc = upper as char;
        let lc = (upper - b'A' + b'a') as char;
        assert_eq!(mgr.downcase_char(uc), lc);
    }
}

#[test]
fn roundtrip_upcase_downcase() {
    let mgr = CaseTableManager::new();
    for lower in b'a'..=b'z' {
        let lc = lower as char;
        let uc = mgr.upcase_char(lc);
        let back = mgr.downcase_char(uc);
        assert_eq!(back, lc);
    }
}

#[test]
fn string_roundtrip() {
    let mgr = CaseTableManager::new();
    let original = "Hello World";
    let upper = mgr.upcase_string(original);
    let lower = mgr.downcase_string(&upper);
    assert_eq!(lower, "hello world");
}

#[test]
fn non_ascii_chars_unchanged() {
    let mgr = CaseTableManager::new();
    // Non-ASCII characters should pass through unchanged with the ASCII table.
    assert_eq!(mgr.upcase_char('\u{00e9}'), '\u{00e9}'); // e-acute
    assert_eq!(mgr.downcase_char('\u{00c9}'), '\u{00c9}'); // E-acute
    assert_eq!(mgr.upcase_string("\u{00e9}"), "\u{00e9}");
}

#[test]
fn is_case_table_on_short_vector() {
    // A vector too short to be a char-table.
    let v = Value::vector(vec![Value::Symbol(intern(CT_CHAR_TABLE_TAG)), Value::Nil]);
    assert!(!is_case_table(&v));
}

#[test]
fn is_case_table_wrong_subtype() {
    // A char-table with a different subtype is NOT a case table.
    let v = build_char_table("syntax-table", &[], Value::Nil, &[]);
    assert!(!is_case_table(&v));
}

#[test]
fn standard_case_table_is_char_table() {
    use super::super::chartable::is_char_table;
    let ct = make_standard_case_table_value();
    assert!(is_char_table(&ct));
    assert!(is_case_table(&ct));
}

#[test]
fn standard_case_table_has_extra_slots() {
    let ct = make_standard_case_table_value();
    if let Value::Vector(arc) = ct {
        let vec = with_heap(|h| h.get_vector(arc).clone());
        // extra count should be 3
        assert!(matches!(vec[CT_EXTRA_COUNT], Value::Int(3)));
        // extra slots 0,1,2 should be char-tables (subsidiary tables)
        use super::super::chartable::is_char_table;
        assert!(is_char_table(&vec[CT_EXTRA_START])); // upcase
        assert!(is_char_table(&vec[CT_EXTRA_START + 1])); // canonicalize
        assert!(is_char_table(&vec[CT_EXTRA_START + 2])); // equivalences
    } else {
        panic!("expected vector");
    }
}
