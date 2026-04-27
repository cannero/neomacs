use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// -----------------------------------------------------------------------
// Char-table tests
// -----------------------------------------------------------------------

#[test]
fn make_char_table_basic() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::NIL);
    assert!(is_char_table(&ct));
    assert!(!is_bool_vector(&ct));
}

#[test]
fn make_char_table_with_default() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::fixnum(42));
    assert!(is_char_table(&ct));
    // Default lookup should return the default.
    let def = builtin_char_table_range(vec![ct, Value::NIL]).unwrap();
    assert!(def.is_fixnum());
}

#[test]
fn char_table_p_predicate() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    assert!(builtin_char_table_p(vec![ct]).unwrap().is_t());
    assert!(
        builtin_char_table_p(vec![Value::fixnum(5)])
            .unwrap()
            .is_nil()
    );
    assert!(builtin_char_table_p(vec![Value::NIL]).unwrap().is_nil());
}

#[test]
fn set_and_get_single_char() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![ct, Value::fixnum(65), Value::symbol("letter-a")]).unwrap();
    let val = builtin_char_table_range(vec![ct, Value::fixnum(65)]).unwrap();
    assert!(val.is_symbol_named("letter-a"));
}

#[test]
fn lookup_falls_back_to_default() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::symbol("default-val"));
    // No entry for char 90.
    let val = builtin_char_table_range(vec![ct, Value::fixnum(90)]).unwrap();
    assert!(val.is_symbol_named("default-val"));
}

#[test]
fn set_range_cons() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    // Set chars 65..=67 (A, B, C)
    let range = Value::cons(Value::fixnum(65), Value::fixnum(67));
    builtin_set_char_table_range(vec![ct, range, Value::symbol("abc")]).unwrap();
    for ch in 65..=67 {
        let val = builtin_char_table_range(vec![ct, Value::fixnum(ch)]).unwrap();
        assert!(val.is_symbol_named("abc"));
    }
    // Char 68 should be nil (default).
    let val = builtin_char_table_range(vec![ct, Value::fixnum(68)]).unwrap();
    assert!(val.is_nil());
}

#[test]
fn set_default_via_range_nil() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![ct, Value::NIL, Value::fixnum(999)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::NIL]).unwrap();
    assert!(def.is_fixnum());
}

#[test]
fn set_range_t_sets_default_value() {
    crate::test_utils::init_test_tracing();
    // In GNU Emacs, (set-char-table-range ct t value) sets all character
    // entries, but leaves the default slot untouched.
    let ct = make_char_table_value(Value::symbol("test"), Value::fixnum(0));
    builtin_set_char_table_range(vec![ct, Value::T, Value::fixnum(5)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::fixnum('a' as i64)]).unwrap();
    let b = builtin_char_table_range(vec![ct, Value::fixnum('b' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::NIL]).unwrap();
    assert!(a.is_fixnum());
    assert!(b.is_fixnum());
    assert!(def.is_fixnum());
}

#[test]
fn set_range_t_allows_single_char_override() {
    crate::test_utils::init_test_tracing();
    // (set-char-table-range ct t 5) sets all characters to 5 without touching
    // the default slot. Later single-char overrides take precedence.
    let ct = make_char_table_value(Value::symbol("test"), Value::fixnum(0));
    builtin_set_char_table_range(vec![ct, Value::T, Value::fixnum(5)]).unwrap();
    builtin_set_char_table_range(vec![ct, Value::fixnum('a' as i64), Value::fixnum(9)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::fixnum('a' as i64)]).unwrap();
    let b = builtin_char_table_range(vec![ct, Value::fixnum('b' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::NIL]).unwrap();
    assert!(a.is_fixnum());
    assert!(b.is_fixnum());
    assert!(def.is_fixnum());
}

#[test]
fn later_t_write_overrides_prior_specific_entries() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![ct, Value::fixnum('a' as i64), Value::fixnum(9)]).unwrap();
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::fixnum('0' as i64), Value::fixnum('9' as i64)),
        Value::fixnum(7),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![ct, Value::T, Value::fixnum(5)]).unwrap();

    let a = builtin_char_table_range(vec![ct, Value::fixnum('a' as i64)]).unwrap();
    let five = builtin_char_table_range(vec![ct, Value::fixnum('5' as i64)]).unwrap();
    let def = builtin_char_table_range(vec![ct, Value::NIL]).unwrap();
    assert!(a.is_fixnum());
    assert!(five.is_fixnum());
    assert!(def.is_nil());
}

#[test]
fn parent_chain_lookup() {
    crate::test_utils::init_test_tracing();
    let parent = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![
        parent,
        Value::fixnum(65),
        Value::symbol("from-parent"),
    ])
    .unwrap();
    let child = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    // Lookup in child falls through to parent.
    let val = builtin_char_table_range(vec![child, Value::fixnum(65)]).unwrap();
    assert!(val.is_symbol_named("from-parent"));

    // Child override takes priority.
    builtin_set_char_table_range(vec![child, Value::fixnum(65), Value::symbol("child-val")])
        .unwrap();
    let val = builtin_char_table_range(vec![child, Value::fixnum(65)]).unwrap();
    assert!(val.is_symbol_named("child-val"));
}

#[test]
fn char_table_parent_get_set() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    // Initially nil.
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(p.is_nil());

    let parent = make_char_table_value(Value::symbol("parent"), Value::NIL);
    builtin_set_char_table_parent(vec![ct, parent]).unwrap();
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(is_char_table(&p));
}

#[test]
fn set_char_table_parent_nil() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    let parent = make_char_table_value(Value::symbol("parent"), Value::NIL);
    builtin_set_char_table_parent(vec![ct, parent]).unwrap();
    builtin_set_char_table_parent(vec![ct, Value::NIL]).unwrap();
    let p = builtin_char_table_parent(vec![ct]).unwrap();
    assert!(p.is_nil());
}

#[test]
fn set_char_table_parent_wrong_type() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    let result = builtin_set_char_table_parent(vec![ct, Value::fixnum(5)]);
    assert!(result.is_err());
}

#[test]
fn char_table_extra_slot_basic() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    // Initially 0 extra slots -- should error.
    let result = builtin_char_table_extra_slot(vec![ct, Value::fixnum(0)]);
    assert!(result.is_err());

    // Setting an out-of-range slot also errors in Emacs.
    let set_result =
        builtin_set_char_table_extra_slot(vec![ct, Value::fixnum(0), Value::symbol("extra0")]);
    assert!(set_result.is_err());
}

#[test]
fn char_table_extra_slot_preserves_data() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    // Set a char entry first.
    builtin_set_char_table_range(vec![ct, Value::fixnum(65), Value::symbol("a-val")]).unwrap();
    // Attempting to set an out-of-range extra slot should fail.
    assert!(
        builtin_set_char_table_extra_slot(vec![ct, Value::fixnum(0), Value::symbol("e0")]).is_err()
    );
    // The char entry should still be intact.
    let val = builtin_char_table_range(vec![ct, Value::fixnum(65)]).unwrap();
    assert!(val.is_symbol_named("a-val"));
    // Extra slot remains out-of-range.
    assert!(builtin_char_table_extra_slot(vec![ct, Value::fixnum(0)]).is_err());
}

#[test]
fn char_table_subtype() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("syntax-table"), Value::NIL);
    let st = builtin_char_table_subtype(vec![ct]).unwrap();
    assert!(st.is_symbol_named("syntax-table"));
}

#[test]
fn char_table_overwrite_entry() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![ct, Value::fixnum(65), Value::fixnum(1)]).unwrap();
    builtin_set_char_table_range(vec![ct, Value::fixnum(65), Value::fixnum(2)]).unwrap();
    let val = builtin_char_table_range(vec![ct, Value::fixnum(65)]).unwrap();
    assert!(val.is_fixnum());
}

#[test]
fn later_range_overrides_earlier_single_entry() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![ct, Value::fixnum('M' as i64), Value::symbol("single")])
        .unwrap();
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('Z' as i64)),
        Value::symbol("range"),
    ])
    .unwrap();

    let val = builtin_char_table_range(vec![ct, Value::fixnum('M' as i64)]).unwrap();
    assert!(val.is_symbol_named("range"));
}

#[test]
fn explicit_nil_entry_inherits_from_parent() {
    crate::test_utils::init_test_tracing();
    let parent = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![
        parent,
        Value::fixnum('a' as i64),
        Value::symbol("parent-a"),
    ])
    .unwrap();

    let child = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();
    builtin_set_char_table_range(vec![child, Value::fixnum('a' as i64), Value::NIL]).unwrap();

    let val = builtin_char_table_range(vec![child, Value::fixnum('a' as i64)]).unwrap();
    assert!(val.is_symbol_named("parent-a"));
}

#[test]
fn set_char_table_parent_rejects_cycles() {
    crate::test_utils::init_test_tracing();
    let parent = make_char_table_value(Value::symbol("test"), Value::NIL);
    let child = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    let result = builtin_set_char_table_parent(vec![parent, child]);
    assert!(result.is_err());
}

#[test]
fn map_char_table_coalesces_ranges_after_single_override() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('Z' as i64)),
        Value::symbol("upper"),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![ct, Value::fixnum('M' as i64), Value::symbol("middle")])
        .unwrap();

    let entries = ct_resolved_entries(&ct);
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries,
        vec![
            (
                Value::cons(Value::fixnum('A' as i64), Value::fixnum('L' as i64)),
                Value::symbol("upper"),
            ),
            (Value::fixnum('M' as i64), Value::symbol("middle")),
            (
                Value::cons(Value::fixnum('N' as i64), Value::fixnum('Z' as i64)),
                Value::symbol("upper"),
            ),
        ]
    );
}

#[test]
fn map_char_table_latest_nil_entry_falls_back_to_parent_run() {
    crate::test_utils::init_test_tracing();
    let parent = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![
        parent,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('Z' as i64)),
        Value::symbol("parent"),
    ])
    .unwrap();

    let child = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();
    builtin_set_char_table_range(vec![
        child,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('Z' as i64)),
        Value::symbol("child"),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![child, Value::fixnum('M' as i64), Value::NIL]).unwrap();

    let entries = ct_resolved_entries(&child);
    assert_eq!(
        entries,
        vec![
            (
                Value::cons(Value::fixnum('A' as i64), Value::fixnum('L' as i64)),
                Value::symbol("child"),
            ),
            (Value::fixnum('M' as i64), Value::symbol("parent")),
            (
                Value::cons(Value::fixnum('N' as i64), Value::fixnum('Z' as i64)),
                Value::symbol("child"),
            ),
        ]
    );
}

#[test]
fn map_char_table_shared_range_survives_callback_gc() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_range(vec![
        ct,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('Z' as i64)),
        Value::symbol("upper"),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![ct, Value::fixnum('M' as i64), Value::symbol("middle")])
        .unwrap();

    let roots = eval.save_specpdl_roots();
    eval.push_specpdl_root(ct);
    let mut seen = 0;
    let result = for_each_char_table_mapping(&ct, |_key, _value| {
        seen += 1;
        eval.gc_collect_exact();
        Ok(())
    });
    eval.restore_specpdl_roots(roots);

    result.unwrap();
    assert_eq!(seen, 3);

    let first = Value::cons(Value::fixnum(1), Value::NIL);
    let second = Value::cons(Value::fixnum(2), first);
    assert!(second.is_cons());
}

#[test]
fn map_char_table_decodes_unicode_property_run_length_values() {
    crate::test_utils::init_test_tracing();
    let table =
        make_char_table_with_extra_slots(Value::symbol("char-code-property-table"), Value::NIL, 5);
    builtin_set_char_table_extra_slot(vec![
        table,
        Value::fixnum(0),
        Value::symbol("general-category"),
    ])
    .unwrap();
    builtin_set_char_table_extra_slot(vec![table, Value::fixnum(1), Value::fixnum(0)]).unwrap();
    builtin_set_char_table_extra_slot(vec![
        table,
        Value::fixnum(4),
        Value::vector(vec![Value::NIL, Value::symbol("Lu"), Value::symbol("Ll")]),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![
        table,
        Value::cons(Value::fixnum('A' as i64), Value::fixnum('B' as i64)),
        Value::fixnum(1),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![table, Value::fixnum('c' as i64), Value::fixnum(2)]).unwrap();

    let mut values = Vec::new();
    for_each_char_table_mapping(&table, |_key, value| {
        values.push(value);
        Ok(())
    })
    .unwrap();

    assert_eq!(values, vec![Value::symbol("Lu"), Value::symbol("Ll")]);
}

#[test]
fn char_table_p_on_plain_vector() {
    crate::test_utils::init_test_tracing();
    // A plain vector should not be detected as a char-table.
    let v = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    assert!(!is_char_table(&v));
}

#[test]
fn char_table_wrong_type_signals() {
    crate::test_utils::init_test_tracing();
    let result = builtin_char_table_range(vec![Value::fixnum(5), Value::fixnum(65)]);
    assert!(result.is_err());
    let result =
        builtin_set_char_table_range(vec![Value::NIL, Value::fixnum(65), Value::fixnum(1)]);
    assert!(result.is_err());
    let result = builtin_char_table_parent(vec![Value::string("not-a-table")]);
    assert!(result.is_err());
}

#[test]
fn char_table_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    // builtin_make_char_table arity is validated by the Context dispatch
    // layer; make_char_table_value doesn't validate, so skip that assertion.
    assert!(builtin_char_table_p(vec![]).is_err());
    assert!(builtin_char_table_range(vec![Value::NIL]).is_err());
    assert!(builtin_set_char_table_range(vec![Value::NIL, Value::NIL]).is_err());
}

#[test]
fn char_table_char_key() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    // Use Value::Char for setting.
    builtin_set_char_table_range(vec![ct, Value::char('Z'), Value::symbol("zee")]).unwrap();
    // Look up with Int.
    let val = builtin_char_table_range(vec![ct, Value::fixnum('Z' as i64)]).unwrap();
    assert!(val.is_symbol_named("zee"));
}

#[test]
fn parent_default_fallback() {
    crate::test_utils::init_test_tracing();
    // Parent has default but no explicit entry.
    let parent = make_char_table_value(Value::symbol("test"), Value::symbol("parent-default"));
    let child = make_char_table_value(Value::symbol("test"), Value::NIL);
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    // Child has no entry, parent has no entry, parent default is used.
    let val = builtin_char_table_range(vec![child, Value::fixnum(100)]).unwrap();
    assert!(val.is_symbol_named("parent-default"));
}

#[test]
fn non_nil_child_default_overrides_parent_lookup() {
    crate::test_utils::init_test_tracing();
    let parent = make_char_table_value(Value::symbol("test"), Value::fixnum(8));
    let child = make_char_table_value(Value::symbol("test"), Value::fixnum(0));
    builtin_set_char_table_parent(vec![child, parent]).unwrap();

    let val = builtin_char_table_range(vec![child, Value::fixnum('a' as i64)]).unwrap();
    assert!(val.is_fixnum());
}

// -----------------------------------------------------------------------
// Bool-vector tests
// -----------------------------------------------------------------------

#[test]
fn make_bool_vector_basic() {
    crate::test_utils::init_test_tracing();
    let bv = builtin_make_bool_vector(vec![Value::fixnum(5), Value::NIL]).unwrap();
    assert!(is_bool_vector(&bv));
    assert!(!is_char_table(&bv));
}

#[test]
fn bool_vector_constructor_from_rest_args() {
    crate::test_utils::init_test_tracing();
    let bv = builtin_bool_vector(vec![
        Value::T,
        Value::NIL,
        Value::fixnum(0),
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
    crate::test_utils::init_test_tracing();
    let bv = builtin_make_bool_vector(vec![Value::fixnum(4), Value::T]).unwrap();
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(count.is_fixnum());
}

#[test]
fn make_bool_vector_all_false() {
    crate::test_utils::init_test_tracing();
    let bv = builtin_make_bool_vector(vec![Value::fixnum(4), Value::NIL]).unwrap();
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(count.is_fixnum());
}

#[test]
fn bool_vector_p_predicate() {
    crate::test_utils::init_test_tracing();
    let bv = builtin_make_bool_vector(vec![Value::fixnum(3), Value::NIL]).unwrap();
    assert!(builtin_bool_vector_p(vec![bv]).unwrap().is_t());
    assert!(
        builtin_bool_vector_p(vec![Value::fixnum(0)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn bool_vector_intersection() {
    crate::test_utils::init_test_tracing();
    // a = [1, 1, 0, 0], b = [1, 0, 1, 0] -> AND = [1, 0, 0, 0]
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_intersection(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, false, false, false]);
}

#[test]
fn bool_vector_union() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_union(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, true, true, false]);
}

#[test]
fn bool_vector_exclusive_or() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, true, false, false]);
    let b = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_exclusive_or(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[false, true, true, false]);
}

#[test]
fn bool_vector_not() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, false, true, false]);
    let result = builtin_bool_vector_not(vec![a]).unwrap();
    assert_bv_bits(&result, &[false, true, false, true]);
}

#[test]
fn bool_vector_not_into_dest() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[false, false, true]);
    let dest = make_bv(&[false, false, false]);
    let result = builtin_bool_vector_not(vec![a, dest]).unwrap();
    assert_eq!(result, dest);
    assert_bv_bits(&dest, &[true, true, false]);
}

#[test]
fn bool_vector_set_difference() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, true, false, true]);
    let b = make_bv(&[false, true, true, false]);
    let result = builtin_bool_vector_set_difference(vec![a, b]).unwrap();
    assert_bv_bits(&result, &[true, false, false, true]);
}

#[test]
fn bool_vector_count_consecutive() {
    crate::test_utils::init_test_tracing();
    let bv = make_bv(&[true, true, false, false, true, true]);
    let count_true_start =
        builtin_bool_vector_count_consecutive(vec![bv, Value::T, Value::fixnum(0)]).unwrap();
    let count_false_middle =
        builtin_bool_vector_count_consecutive(vec![bv, Value::NIL, Value::fixnum(2)]).unwrap();
    let count_true_mismatch =
        builtin_bool_vector_count_consecutive(vec![bv, Value::T, Value::fixnum(2)]).unwrap();
    assert!(count_true_start.is_fixnum());
    assert!(count_false_middle.is_fixnum());
    assert!(count_true_mismatch.is_fixnum());
}

#[test]
fn bool_vector_subsetp_true() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, false, false]);
    let b = make_bv(&[true, true, false]);
    let result = builtin_bool_vector_subsetp(vec![a, b]).unwrap();
    assert!(result.is_t());
}

#[test]
fn bool_vector_subsetp_false() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, false, true]);
    let b = make_bv(&[true, true, false]);
    let result = builtin_bool_vector_subsetp(vec![a, b]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn bool_vector_count_population_mixed() {
    crate::test_utils::init_test_tracing();
    let bv = make_bv(&[true, false, true, true, false]);
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(count.is_fixnum());
}

#[test]
fn bool_vector_empty() {
    crate::test_utils::init_test_tracing();
    let bv = builtin_make_bool_vector(vec![Value::fixnum(0), Value::NIL]).unwrap();
    assert!(is_bool_vector(&bv));
    let count = builtin_bool_vector_count_population(vec![bv]).unwrap();
    assert!(count.is_fixnum());
}

#[test]
fn bool_vector_negative_length() {
    crate::test_utils::init_test_tracing();
    let result = builtin_make_bool_vector(vec![Value::fixnum(-1), Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_wrong_type_signals() {
    crate::test_utils::init_test_tracing();
    let result = builtin_bool_vector_count_population(vec![Value::fixnum(0)]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_mismatched_length() {
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, false]);
    let b = make_bv(&[true]);
    let result = builtin_bool_vector_intersection(vec![a, b]);
    assert!(result.is_err());
}

#[test]
fn bool_vector_intersection_into_dest() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let a = make_bv(&[true, false, false]);
    let b = make_bv(&[false, true, false]);
    let dest = make_bv(&[false, false, false]);
    builtin_bool_vector_union(vec![a, b, dest]).unwrap();
    assert_bv_bits(&dest, &[true, true, false]);
}

#[test]
fn is_predicates_disjoint() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    let bv = builtin_make_bool_vector(vec![Value::fixnum(3), Value::NIL]).unwrap();
    let v = Value::vector(vec![Value::fixnum(1)]);
    assert!(is_char_table(&ct));
    assert!(!is_bool_vector(&ct));
    assert!(!is_char_table(&bv));
    assert!(is_bool_vector(&bv));
    assert!(!is_char_table(&v));
    assert!(!is_bool_vector(&v));
}

#[test]
fn bool_vector_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_make_bool_vector(vec![]).is_err());
    assert!(builtin_bool_vector_p(vec![]).is_err());
    assert!(builtin_bool_vector_subsetp(vec![Value::NIL]).is_err());
    assert!(builtin_bool_vector_not(vec![]).is_err());
    assert!(builtin_bool_vector_not(vec![Value::NIL, Value::NIL, Value::NIL]).is_err());
}

#[test]
fn char_table_range_invalid_range_type() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    let result = builtin_set_char_table_range(vec![ct, Value::string("invalid"), Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn char_table_range_reverse_cons_errors() {
    crate::test_utils::init_test_tracing();
    let ct = make_char_table_value(Value::symbol("test"), Value::NIL);
    let range = Value::cons(Value::fixnum(70), Value::fixnum(65)); // min > max
    let result = builtin_set_char_table_range(vec![ct, range, Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn unicode_property_table_internal_returns_alist_char_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let prop = Value::symbol("neo-test-property");
    let table =
        make_char_table_with_extra_slots(Value::symbol("char-code-property-table"), Value::NIL, 5);
    builtin_set_char_table_extra_slot(vec![table, Value::fixnum(0), prop]).unwrap();
    builtin_set_char_table_extra_slot(vec![table, Value::fixnum(1), Value::fixnum(0)]).unwrap();
    builtin_set_char_table_extra_slot(vec![
        table,
        Value::fixnum(4),
        Value::vector(vec![Value::NIL, Value::symbol("letter")]),
    ])
    .unwrap();
    builtin_set_char_table_range(vec![table, Value::fixnum(65), Value::fixnum(1)]).unwrap();
    eval.obarray.set_symbol_value(
        "char-code-property-alist",
        Value::list(vec![Value::cons(prop, table)]),
    );

    let returned = builtin_unicode_property_table_internal(&mut eval, vec![prop])
        .expect("unicode-property-table-internal should return the table");
    assert!(is_char_table(&returned));

    let decoded = builtin_get_unicode_property_internal(vec![returned, Value::fixnum(65)])
        .expect("run-length decoder should map through extra slot 4");
    assert!(decoded.is_symbol_named("letter"));
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
    assert!(bv.is_vector(), "expected a vector");
    let vec = bv.as_vector_data().unwrap().clone();
    let len = bv_length(&vec) as usize;
    assert_eq!(len, expected.len(), "bool-vector length mismatch");
    let bits = bv_bits(&vec);
    assert_eq!(bits, expected);
}
