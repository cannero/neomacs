use super::*;

// -----------------------------------------------------------------------
// Char-table tests
// -----------------------------------------------------------------------

#[test]
fn make_char_table_basic() {
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::Nil);
    assert!(is_char_table(&ct));
    assert!(!is_bool_vector(&ct));
}

#[test]
fn make_char_table_with_default() {
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::Int(42));
    assert!(is_char_table(&ct));
    // Default lookup should return the default.
    let def = builtin_char_table_range(vec![ct, Value::Nil]).unwrap();
    assert!(matches!(def, Value::Int(42)));
}

#[test]
fn char_table_p_predicate() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    assert!(matches!(
        builtin_char_table_p(vec![ct]).unwrap(),
        Value::True
    ));
    assert!(matches!(
        builtin_char_table_p(vec![Value::Int(5)]).unwrap(),
        Value::Nil
    ));
    assert!(matches!(
        builtin_char_table_p(vec![Value::Nil]).unwrap(),
        Value::Nil
    ));
}

#[test]
fn set_and_get_single_char() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![ct, Value::Int(65), Value::symbol("letter-a")]).unwrap();
    let val = builtin_char_table_range(vec![ct, Value::Int(65)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="letter-a"));
}

#[test]
fn lookup_falls_back_to_default() {
    let ct = make_char_table_value(Value::symbol("test"), Value::symbol("default-val"));
    // No entry for char 90.
    let val = builtin_char_table_range(vec![ct, Value::Int(90)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="default-val"));
}

#[test]
fn set_range_cons() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    // Set chars 65..=67 (A, B, C)
    let range = Value::cons(Value::Int(65), Value::Int(67));
    builtin_set_char_table_range(vec![ct, range, Value::symbol("abc")]).unwrap();
    for ch in 65..=67 {
        let val = builtin_char_table_range(vec![ct, Value::Int(ch)]).unwrap();
        assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="abc"));
    }
    // Char 68 should be nil (default).
    let val = builtin_char_table_range(vec![ct, Value::Int(68)]).unwrap();
    assert!(val.is_nil());
}

#[test]
fn set_default_via_range_nil() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![ct, Value::Nil, Value::Int(999)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::Nil]).unwrap();
    assert!(matches!(def, Value::Int(999)));
}

#[test]
fn set_range_t_sets_default_value() {
    // In GNU Emacs, (set-char-table-range ct t value) sets all character
    // entries, but leaves the default slot untouched.
    let ct = make_char_table_value(Value::symbol("test"), Value::Int(0));
    builtin_set_char_table_range(vec![ct, Value::True, Value::Int(5)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::Int('a' as i64)]).unwrap();
    let b = builtin_char_table_range(vec![ct, Value::Int('b' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::Nil]).unwrap();
    assert!(matches!(a, Value::Int(5)));
    assert!(matches!(b, Value::Int(5)));
    assert!(matches!(def, Value::Int(0)));
}

#[test]
fn set_range_t_allows_single_char_override() {
    // (set-char-table-range ct t 5) sets all characters to 5 without touching
    // the default slot. Later single-char overrides take precedence.
    let ct = make_char_table_value(Value::symbol("test"), Value::Int(0));
    builtin_set_char_table_range(vec![ct, Value::True, Value::Int(5)]).unwrap();
    builtin_set_char_table_range(vec![ct, Value::Int('a' as i64), Value::Int(9)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::Int('a' as i64)]).unwrap();
    let b = builtin_char_table_range(vec![ct, Value::Int('b' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::Nil]).unwrap();
    assert!(matches!(a, Value::Int(9)));
    assert!(matches!(b, Value::Int(5)));
    assert!(matches!(def, Value::Int(0)));
}

#[test]
fn later_t_write_overrides_prior_specific_entries() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![ct, Value::Int('a' as i64), Value::Int(9)]).unwrap();
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::Int('0' as i64), Value::Int('9' as i64)),
        Value::Int(7),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![ct, Value::True, Value::Int(5)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::Int('a' as i64)]).unwrap();
    let five = builtin_char_table_range(vec![ct, Value::Int('5' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::Nil]).unwrap();
    assert!(matches!(a, Value::Int(5)));
    assert!(matches!(five, Value::Int(5)));
    assert!(def.is_nil());
}

#[test]
fn parent_chain_lookup() {
    let parent = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![parent, Value::Int(65), Value::symbol("from-parent")])
        .unwrap();
    let child = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    // Lookup in child falls through to parent.
    let val = builtin_char_table_range(vec![child, Value::Int(65)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="from-parent"));

    // Child override takes priority.
    builtin_set_char_table_range(vec![child, Value::Int(65), Value::symbol("child-val")]).unwrap();
    let val = builtin_char_table_range(vec![child, Value::Int(65)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="child-val"));
}

#[test]
fn char_table_parent_get_set() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    // Initially nil.
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(p.is_nil());

    let parent = make_char_table_value(Value::symbol("parent"), Value::Nil);
    builtin_set_char_table_parent(vec![ct, parent]).unwrap();
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(is_char_table(&p));
}

#[test]
fn set_char_table_parent_nil() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    let parent = make_char_table_value(Value::symbol("parent"), Value::Nil);
    builtin_set_char_table_parent(vec![ct, parent]).unwrap();
    builtin_set_char_table_parent(vec![ct, Value::Nil]).unwrap();
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(p.is_nil());
}

#[test]
fn set_char_table_parent_wrong_type() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    let result = builtin_set_char_table_parent(vec![ct, Value::Int(5)]);
    assert!(result.is_err());
}

#[test]
fn char_table_extra_slot_basic() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    // Initially 0 extra slots -- should error.
    let result = builtin_char_table_extra_slot(vec![ct, Value::Int(0)]);
    assert!(result.is_err());

    // Setting an out-of-range slot also errors in Emacs.
    let set_result =
        builtin_set_char_table_extra_slot(vec![ct, Value::Int(0), Value::symbol("extra0")]);
    assert!(set_result.is_err());
}

#[test]
fn char_table_extra_slot_preserves_data() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    // Set a char entry first.
    builtin_set_char_table_range(vec![ct, Value::Int(65), Value::symbol("a-val")]).unwrap();
    // Attempting to set an out-of-range extra slot should fail.
    assert!(
        builtin_set_char_table_extra_slot(vec![ct, Value::Int(0), Value::symbol("e0")]).is_err()
    );
    // The char entry should still be intact.
    let val = builtin_char_table_range(vec![ct, Value::Int(65)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="a-val"));
    // Extra slot remains out-of-range.
    assert!(builtin_char_table_extra_slot(vec![ct, Value::Int(0)]).is_err());
}

#[test]
fn char_table_subtype() {
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::Nil);
    let st = builtin_char_table_subtype(vec![ct]).unwrap();
    assert!(matches!(st, Value::Symbol(ref id) if resolve_sym(*id) =="syntax-table"));
}

#[test]
fn char_table_overwrite_entry() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![ct, Value::Int(65), Value::Int(1)]).unwrap();
    builtin_set_char_table_range(vec![ct, Value::Int(65), Value::Int(2)]).unwrap();
    let val = builtin_char_table_range(vec![ct, Value::Int(65)]).unwrap();
    assert!(matches!(val, Value::Int(2)));
}

#[test]
fn later_range_overrides_earlier_single_entry() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![ct, Value::Int('M' as i64), Value::symbol("single")])
        .unwrap();
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::Int('A' as i64), Value::Int('Z' as i64)),
        Value::symbol("range"),
    ])
    .unwrap();

    let val = builtin_char_table_range(vec![ct, Value::Int('M' as i64)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) == "range"));
}

#[test]
fn explicit_nil_entry_inherits_from_parent() {
    let parent = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![
        parent,
        Value::Int('a' as i64),
        Value::symbol("parent-a"),
    ])
    .unwrap();

    let child = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();
    builtin_set_char_table_range(vec![child, Value::Int('a' as i64), Value::Nil]).unwrap();

    let val = builtin_char_table_range(vec![child, Value::Int('a' as i64)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) == "parent-a"));
}

#[test]
fn set_char_table_parent_rejects_cycles() {
    let parent = make_char_table_value(Value::symbol("test"), Value::Nil);
    let child = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    let result = builtin_set_char_table_parent(vec![parent, child]);
    assert!(result.is_err());
}

#[test]
fn map_char_table_coalesces_ranges_after_single_override() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::Int('A' as i64), Value::Int('Z' as i64)),
        Value::symbol("upper"),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![ct, Value::Int('M' as i64), Value::symbol("middle")])
        .unwrap();

    let entries = ct_resolved_entries(&ct);
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries,
        vec![
            (
                Value::cons(Value::Int('A' as i64), Value::Int('L' as i64)),
                Value::symbol("upper"),
            ),
            (Value::Int('M' as i64), Value::symbol("middle")),
            (
                Value::cons(Value::Int('N' as i64), Value::Int('Z' as i64)),
                Value::symbol("upper"),
            ),
        ]
    );
}

#[test]
fn char_table_p_on_plain_vector() {
    // A plain vector should not be detected as a char-table.
    let v = Value::vector(vec![Value::Int(1), Value::Int(2)]);
    assert!(!is_char_table(&v));
}

#[test]
fn char_table_wrong_type_signals() {
    let result = builtin_char_table_range(vec![Value::Int(5), Value::Int(65)]);
    assert!(result.is_err());
    let result = builtin_set_char_table_range(vec![Value::Nil, Value::Int(65), Value::Int(1)]);
    assert!(result.is_err());
    let result = builtin_char_table_parent(vec![Value::string("not-a-table")]);
    assert!(result.is_err());
}

#[test]
fn char_table_wrong_arg_count() {
    // builtin_make_char_table arity is validated by the Context dispatch
    // layer; make_char_table_value doesn't validate, so skip that assertion.
    assert!(builtin_char_table_p(vec![]).is_err());
    assert!(builtin_char_table_range(vec![Value::Nil]).is_err());
    assert!(builtin_set_char_table_range(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn char_table_char_key() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    // Use Value::Char for setting.
    builtin_set_char_table_range(vec![ct, Value::Char('Z'), Value::symbol("zee")]).unwrap();
    // Look up with Int.
    let val = builtin_char_table_range(vec![ct, Value::Int('Z' as i64)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="zee"));
}

#[test]
fn parent_default_fallback() {
    // Parent has default but no explicit entry.
    let parent = make_char_table_value(Value::symbol("test"), Value::symbol("parent-default"));
    let child = make_char_table_value(Value::symbol("test"), Value::Nil);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    // Child has no entry, parent has no entry, parent default is used.
    let val = builtin_char_table_range(vec![child, Value::Int(100)]).unwrap();
    assert!(matches!(val, Value::Symbol(ref id) if resolve_sym(*id) =="parent-default"));
}

#[test]
fn non_nil_child_default_overrides_parent_lookup() {
    let parent = make_char_table_value(Value::symbol("test"), Value::Int(8));
    let child = make_char_table_value(Value::symbol("test"), Value::Int(0));
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    let val = builtin_char_table_range(vec![child, Value::Int('a' as i64)]).unwrap();
    assert!(matches!(val, Value::Int(0)));
}

// -----------------------------------------------------------------------
// Bool-vector tests
// -----------------------------------------------------------------------

#[test]
fn make_bool_vector_basic() {
    let bv = builtin_make_bool_vector(vec![Value::Int(5), Value::Nil]).unwrap();
    assert!(is_bool_vector(&bv));
    assert!(!is_char_table(&bv));
}

#[test]
fn bool_vector_constructor_from_rest_args() {
    let bv = builtin_bool_vector(vec![
        Value::True,
        Value::Nil,
        Value::Int(0),
        Value::symbol("x"),
    ])
    .unwrap();
    assert!(is_bool_vector(&bv));
    assert_bv_bits(&bv, &[true, false, true, true]);

    let empty = builtin_bool_vector(vec![]).unwrap();
    assert!(is_bool_vector(&empty));
    assert_bv_bits(&empty, &[]);
}

#[test]
fn make_bool_vector_all_true() {
    let bv = builtin_make_bool_vector(vec![Value::Int(4), Value::True]).unwrap();
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(matches!(count, Value::Int(4)));
}

#[test]
fn make_bool_vector_all_false() {
    let bv = builtin_make_bool_vector(vec![Value::Int(4), Value::Nil]).unwrap();
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(matches!(count, Value::Int(0)));
}

#[test]
fn bool_vector_p_predicate() {
    let bv = builtin_make_bool_vector(vec![Value::Int(3), Value::Nil]).unwrap();
    assert!(matches!(
        builtin_bool_vector_p(vec![bv]).unwrap(),
        Value::True
    ));
    assert!(matches!(
        builtin_bool_vector_p(vec![Value::Int(0)]).unwrap(),
        Value::Nil
    ));
}

#[test]
fn bool_vector_intersection() {
    // a = [1, 1, 0, 0], b = [1, 0, 1, 0] -> AND = [1, 0, 0, 0]
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_intersection(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, false, false, false]);
}

#[test]
fn bool_vector_union() {
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_union(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, true, true, false]);
}

#[test]
fn bool_vector_exclusive_or() {
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_exclusive_or(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[false, true, true, false]);
}

#[test]
fn bool_vector_not() {
    let a = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_not(vec![a]).unwrap();
    assert_bv_bits(&result, &[false, true, false, true]);
}

#[test]
fn bool_vector_not_into_dest() {
    let a = make_bv(&[false, false, true]);
    let dest = make_bv(&[false, false, false]);
    let result = builtin_bool_vector_not(vec![a, dest]).unwrap();
    assert_eq!(result, dest);
    assert_bv_bits(&dest, &[true, true, false]);
}

#[test]
fn bool_vector_set_difference() {
    let a = make_bv(&[true, true, false, true]);
    let b = make_bv(&[false, true, true, false]);
    let result = builtin_bool_vector_set_difference(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, false, false, true]);
}

#[test]
fn bool_vector_count_consecutive() {
    let bv = make_bv(&[true, true, false, false, true, true]);
    let count_true_start =
        builtin_bool_vector_count_consecutive(vec![bv, Value::True, Value::Int(0)]).unwrap();
    let count_false_middle =
        builtin_bool_vector_count_consecutive(vec![bv, Value::Nil, Value::Int(2)]).unwrap();
    let count_true_mismatch =
        builtin_bool_vector_count_consecutive(vec![bv, Value::True, Value::Int(2)]).unwrap();
    assert!(matches!(count_true_start, Value::Int(2)));
    assert!(matches!(count_false_middle, Value::Int(2)));
    assert!(matches!(count_true_mismatch, Value::Int(0)));
}

#[test]
fn bool_vector_subsetp_true() {
    let a = make_bv(&[true, false, false]);
    let b = make_bv(&[true, true, false]);
    let result = builtin_bool_vector_subsetp(vec![a, b]).unwrap();
    assert!(matches!(result, Value::True));
}

#[test]
fn bool_vector_subsetp_false() {
    let a = make_bv(&[true, false, true]);
    let b = make_bv(&[true, true, false]);
    let result = builtin_bool_vector_subsetp(vec![a, b]).unwrap();
    assert!(matches!(result, Value::Nil));
}

#[test]
fn bool_vector_count_population_mixed() {
    let bv = make_bv(&[true, false, true, true, false]);
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(matches!(count, Value::Int(3)));
}

#[test]
fn bool_vector_empty() {
    let bv = builtin_make_bool_vector(vec![Value::Int(0), Value::Nil]).unwrap();
    assert!(is_bool_vector(&bv));
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(matches!(count, Value::Int(0)));
}

#[test]
fn bool_vector_negative_length() {
    let result = builtin_make_bool_vector(vec![Value::Int(-1), Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_wrong_type_signals() {
    let result = builtin_bool_vector_count_population(vec![Value::Int(0)]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_mismatched_length() {
    let a = make_bv(&[true, false]);
    let b = make_bv(&[true]);
    let result = builtin_bool_vector_intersection(vec![a, b]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_intersection_into_dest() {
    let a = make_bv(&[true, true, false]);
    let b = make_bv(&[false, true, true]);
    let dest = make_bv(&[false, false, false]);
    let result = builtin_bool_vector_intersection(vec![a, b, dest]).unwrap();
    // Result should be the same object as dest.
    assert_bv_bits(&result, &[false, true, false]);
    // Dest should have been mutated.
    assert_bv_bits(&dest, &[false, true, false]);
}

#[test]
fn bool_vector_union_into_dest() {
    let a = make_bv(&[true, false, false]);
    let b = make_bv(&[false, true, false]);
    let dest = make_bv(&[false, false, false]);
    builtin_bool_vector_union(vec![a, b, dest]).unwrap();
    assert_bv_bits(&dest, &[true, true, false]);
}

#[test]
fn is_predicates_disjoint() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    let bv = builtin_make_bool_vector(vec![Value::Int(3), Value::Nil]).unwrap();
    let v = Value::vector(vec![Value::Int(1)]);
    assert!(is_char_table(&ct));
    assert!(!is_bool_vector(&ct));
    assert!(!is_char_table(&bv));
    assert!(is_bool_vector(&bv));
    assert!(!is_char_table(&v));
    assert!(!is_bool_vector(&v));
}

#[test]
fn bool_vector_wrong_arg_count() {
    assert!(builtin_make_bool_vector(vec![]).is_err());
    assert!(builtin_bool_vector_p(vec![]).is_err());
    assert!(builtin_bool_vector_subsetp(vec![Value::Nil]).is_err());
    assert!(builtin_bool_vector_not(vec![]).is_err());
    assert!(builtin_bool_vector_not(vec![Value::Nil, Value::Nil, Value::Nil]).is_err());
}

#[test]
fn char_table_range_invalid_range_type() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    let result = builtin_set_char_table_range(vec![ct, Value::string("invalid"), Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn char_table_range_reverse_cons_errors() {
    let ct = make_char_table_value(Value::symbol("test"), Value::Nil);
    let range = Value::cons(Value::Int(70), Value::Int(65)); // min > max
    let result = builtin_set_char_table_range(vec![ct, range, Value::Int(1)]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

/// Build a bool-vector from a slice of bools (test helper).
fn make_bv(bits: &[bool]) -> Value {
    bv_from_bits(bits)
}

/// Assert that a bool-vector has the expected bits.
fn assert_bv_bits(bv: &Value, expected: &[bool]) {
    let arc = match bv {
        Value::Vector(a) => a,
        _ => panic!("expected a vector"),
    };
    let vec = with_heap(|h| h.get_vector(*arc).clone());
    let len = bv_length(&vec) as usize;
    assert_eq!(len, expected.len(), "bool-vector length mismatch");
    let bits = bv_bits(&vec);
    assert_eq!(bits, expected);
}
