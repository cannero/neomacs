use super::*;

// -----------------------------------------------------------------------
// Basic put/get
// -----------------------------------------------------------------------

#[test]
fn put_and_get_basic() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 5, Value::symbol("face"), Value::symbol("bold"));

    assert!(table.get_property(0, Value::symbol("face")).is_some());
    assert!(table.get_property(2, Value::symbol("face")).is_some());
    assert!(table.get_property(4, Value::symbol("face")).is_some());
    assert!(table.get_property(5, Value::symbol("face")).is_none()); // exclusive end
}

#[test]
fn get_property_returns_correct_value() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    let val = table.get_property(5, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "bold")
    );
}

#[test]
fn get_property_nonexistent_name() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    assert!(
        table
            .get_property(5, Value::symbol("syntax-table"))
            .is_none()
    );
}

#[test]
fn get_properties_returns_all() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(0, 10, Value::symbol("help-echo"), Value::string("tooltip"));
    let props = table.get_properties(5);
    assert_eq!(props.len(), 2);
    assert!(props.contains_key(&Value::symbol("face")));
    assert!(props.contains_key(&Value::symbol("help-echo")));
}

#[test]
fn get_property_outside_any_interval() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));
    assert!(table.get_property(0, Value::symbol("face")).is_none());
    assert!(table.get_property(3, Value::symbol("face")).is_none());
    assert!(table.get_property(10, Value::symbol("face")).is_none());
    assert!(table.get_property(15, Value::symbol("face")).is_none());
}

// -----------------------------------------------------------------------
// Overlapping ranges
// -----------------------------------------------------------------------

#[test]
fn overlapping_put_splits_intervals() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(5, 15, Value::symbol("face"), Value::symbol("italic"));

    // [0, 5) should still have "bold"
    let val = table.get_property(3, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "bold")
    );

    // [5, 15) should have "italic" (overwritten)
    let val = table.get_property(7, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "italic")
    );

    let val = table.get_property(12, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "italic")
    );
}

#[test]
fn multiple_properties_on_same_range() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(
        0,
        10,
        Value::symbol("mouse-face"),
        Value::symbol("highlight"),
    );

    let props = table.get_properties(5);
    assert_eq!(props.len(), 2);
}

#[test]
fn put_property_inner_range() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 20, Value::symbol("face"), Value::symbol("default"));
    table.put_property(5, 15, Value::symbol("face"), Value::symbol("bold"));

    let val = table.get_property(3, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "default")
    );

    let val = table.get_property(10, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "bold")
    );

    let val = table.get_property(17, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "default")
    );
}

#[test]
fn put_different_properties_on_overlapping_ranges() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(5, 15, Value::symbol("syntax-table"), Value::fixnum(42));

    // Position 3: only "face"
    let props = table.get_properties(3);
    assert_eq!(props.len(), 1);
    assert!(props.contains_key(&Value::symbol("face")));

    // Position 7: both "face" and "syntax-table"
    let props = table.get_properties(7);
    assert_eq!(props.len(), 2);

    // Position 12: only "syntax-table"
    let props = table.get_properties(12);
    assert_eq!(props.len(), 1);
    assert!(props.contains_key(&Value::symbol("syntax-table")));
}

// -----------------------------------------------------------------------
// Remove
// -----------------------------------------------------------------------

#[test]
fn remove_property_basic() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(0, 10, Value::symbol("help-echo"), Value::string("help"));

    table.remove_property(0, 10, Value::symbol("face"));

    assert!(table.get_property(5, Value::symbol("face")).is_none());
    assert!(table.get_property(5, Value::symbol("help-echo")).is_some());
}

#[test]
fn remove_property_partial_range() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));

    table.remove_property(3, 7, Value::symbol("face"));

    // [0, 3) still has face
    assert!(table.get_property(2, Value::symbol("face")).is_some());
    // [3, 7) no longer has face
    assert!(table.get_property(5, Value::symbol("face")).is_none());
    // [7, 10) still has face
    assert!(table.get_property(8, Value::symbol("face")).is_some());
}

#[test]
fn remove_all_properties_basic() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(0, 10, Value::symbol("help-echo"), Value::string("help"));

    table.remove_all_properties(0, 10);

    assert!(table.get_property(5, Value::symbol("face")).is_none());
    assert!(table.get_property(5, Value::symbol("help-echo")).is_none());
}

#[test]
fn remove_all_properties_partial() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));

    table.remove_all_properties(3, 7);

    assert!(table.get_property(2, Value::symbol("face")).is_some());
    assert!(table.get_property(5, Value::symbol("face")).is_none());
    assert!(table.get_property(8, Value::symbol("face")).is_some());
}

// -----------------------------------------------------------------------
// next/previous property change
// -----------------------------------------------------------------------

#[test]
fn next_property_change_basic() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(15, 20, Value::symbol("face"), Value::symbol("italic"));

    // Before any interval
    assert_eq!(table.next_property_change(0), Some(5));
    // Inside first interval
    assert_eq!(table.next_property_change(7), Some(10));
    // Between intervals
    assert_eq!(table.next_property_change(12), Some(15));
    // Inside second interval
    assert_eq!(table.next_property_change(17), Some(20));
    // After all intervals
    assert_eq!(table.next_property_change(25), None);
}

#[test]
fn next_property_change_at_boundary() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    // At start of interval
    assert_eq!(table.next_property_change(5), Some(10));
}

#[test]
fn previous_property_change_basic() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(15, 20, Value::symbol("face"), Value::symbol("italic"));

    // After second interval
    assert_eq!(table.previous_property_change(25), Some(20));
    // Inside second interval
    assert_eq!(table.previous_property_change(17), Some(15));
    // Between intervals
    assert_eq!(table.previous_property_change(12), Some(10));
    // Inside first interval
    assert_eq!(table.previous_property_change(7), Some(5));
    // Before any interval
    assert_eq!(table.previous_property_change(3), None);
    assert_eq!(table.previous_property_change(0), None);
}

#[test]
fn previous_property_change_at_end() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    // At exclusive end of interval. GNU-verified via
    // `(previous-property-change 11)` with a `[6,11)` interval:
    // GNU returns 6 (the start), i.e. the position at the
    // exclusive end is treated as the scan still inside the run
    // going backward, so the change is at the interval start.
    assert_eq!(table.previous_property_change(10), Some(5));
}

#[test]
fn next_previous_empty_table() {
    crate::test_utils::init_test_tracing();
    let table = TextPropertyTable::new();
    assert_eq!(table.next_property_change(0), None);
    assert_eq!(table.previous_property_change(10), None);
}

// -----------------------------------------------------------------------
// adjust_for_insert
// -----------------------------------------------------------------------

#[test]
fn adjust_insert_shifts_intervals_after() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(10, 20, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_insert(5, 3);

    // Interval should now be [13, 23)
    assert!(table.get_property(12, Value::symbol("face")).is_none());
    assert!(table.get_property(13, Value::symbol("face")).is_some());
    assert!(table.get_property(22, Value::symbol("face")).is_some());
    assert!(table.get_property(23, Value::symbol("face")).is_none());
}

#[test]
fn adjust_insert_splits_spanning_interval_around_plain_inserted_text() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 15, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_insert(10, 5);

    // Plain insert should leave the inserted range [10, 15) without properties.
    assert!(table.get_property(5, Value::symbol("face")).is_some());
    assert!(table.get_property(9, Value::symbol("face")).is_some());
    assert!(table.get_property(10, Value::symbol("face")).is_none());
    assert!(table.get_property(12, Value::symbol("face")).is_none());
    assert!(table.get_property(14, Value::symbol("face")).is_none());
    assert!(table.get_property(15, Value::symbol("face")).is_some());
    assert!(table.get_property(20, Value::symbol("face")).is_none());
}

#[test]
fn adjust_insert_at_interval_start() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_insert(5, 3);

    // Interval should shift to [8, 13)
    assert!(table.get_property(7, Value::symbol("face")).is_none());
    assert!(table.get_property(8, Value::symbol("face")).is_some());
    assert!(table.get_property(12, Value::symbol("face")).is_some());
    assert!(table.get_property(13, Value::symbol("face")).is_none());
}

#[test]
fn adjust_insert_before_all() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_insert(0, 2);

    assert!(table.get_property(7, Value::symbol("face")).is_some());
    assert!(table.get_property(6, Value::symbol("face")).is_none());
}

#[test]
fn adjust_insert_zero_length() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_insert(7, 0);

    // No change
    assert!(table.get_property(5, Value::symbol("face")).is_some());
    assert!(table.get_property(9, Value::symbol("face")).is_some());
    assert!(table.get_property(10, Value::symbol("face")).is_none());
}

// -----------------------------------------------------------------------
// adjust_for_delete
// -----------------------------------------------------------------------

#[test]
fn adjust_delete_shifts_intervals_after() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(10, 20, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(2, 5);

    // 3 bytes deleted before interval; interval becomes [7, 17)
    assert!(table.get_property(6, Value::symbol("face")).is_none());
    assert!(table.get_property(7, Value::symbol("face")).is_some());
    assert!(table.get_property(16, Value::symbol("face")).is_some());
    assert!(table.get_property(17, Value::symbol("face")).is_none());
}

#[test]
fn adjust_delete_removes_contained_interval() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(3, 12);

    // Entire interval was within deleted range
    assert!(table.get_property(5, Value::symbol("face")).is_none());
    assert!(table.get_property(3, Value::symbol("face")).is_none());
}

#[test]
fn adjust_delete_truncates_start() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 15, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(10, 20);

    // Deletion overlaps end of interval; truncated to [5, 10)
    assert!(table.get_property(5, Value::symbol("face")).is_some());
    assert!(table.get_property(9, Value::symbol("face")).is_some());
    assert!(table.get_property(10, Value::symbol("face")).is_none());
}

#[test]
fn adjust_delete_shrinks_spanning_interval() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 20, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(10, 15);

    // Deletion within interval; shrinks to [5, 15)
    assert!(table.get_property(5, Value::symbol("face")).is_some());
    assert!(table.get_property(14, Value::symbol("face")).is_some());
    assert!(table.get_property(15, Value::symbol("face")).is_none());
}

#[test]
fn adjust_delete_overlaps_interval_start() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 15, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(2, 10);

    // Deletion overlaps beginning of interval: [5,15) minus [2,10)
    // After: interval becomes [2, 7) (shifted: start=2, end=15-8=7)
    assert!(table.get_property(2, Value::symbol("face")).is_some());
    assert!(table.get_property(6, Value::symbol("face")).is_some());
    assert!(table.get_property(7, Value::symbol("face")).is_none());
}

#[test]
fn adjust_delete_empty_range() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    table.adjust_for_delete(7, 7);

    // No change
    assert!(table.get_property(5, Value::symbol("face")).is_some());
    assert!(table.get_property(9, Value::symbol("face")).is_some());
}

// -----------------------------------------------------------------------
// Merge adjacent intervals
// -----------------------------------------------------------------------

#[test]
fn merge_adjacent_same_properties() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 5, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("bold"));

    // After put, adjacent intervals with same properties should merge.
    // We can verify by checking that only one interval exists.
    assert!(table.get_property(0, Value::symbol("face")).is_some());
    assert!(table.get_property(7, Value::symbol("face")).is_some());
    assert_eq!(table.intervals_snapshot().len(), 1);

    // next_property_change from 0 should go to 10 (not 5)
    assert_eq!(table.next_property_change(0), Some(10));
    assert_eq!(table.next_property_change(5), Some(10));
    assert_eq!(table.previous_property_change(10), Some(0));
}

#[test]
fn no_merge_different_properties() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 5, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(5, 10, Value::symbol("face"), Value::symbol("italic"));

    // Should remain as two intervals.
    assert_eq!(table.next_property_change(0), Some(5));
    assert_eq!(table.next_property_change(5), Some(10));
}

#[test]
fn adjacent_equal_but_not_eq_values_do_not_merge() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    let left = Value::string("v");
    let right = Value::string("v");
    assert!(!crate::emacs_core::value::eq_value(&left, &right));
    assert!(crate::emacs_core::value::equal_value(&left, &right, 0));

    table.put_property(0, 5, Value::symbol("p"), left);
    table.put_property(5, 10, Value::symbol("p"), right);

    assert_eq!(table.intervals_snapshot().len(), 2);
    assert_eq!(table.next_property_change(0), Some(5));
    assert_eq!(table.next_property_change(5), Some(10));
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn put_property_empty_range() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(5, 5, Value::symbol("face"), Value::symbol("bold"));
    assert!(table.get_property(5, Value::symbol("face")).is_none());
}

#[test]
fn put_property_overwrites_same_name() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(0, 10, Value::symbol("face"), Value::symbol("italic"));

    let val = table.get_property(5, Value::symbol("face")).unwrap();
    assert!(
        val.as_symbol_id()
            .map_or(false, |id| crate::emacs_core::intern::resolve_sym(id)
                == "italic")
    );
}

#[test]
fn multiple_non_contiguous_intervals() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    table.put_property(0, 5, Value::symbol("face"), Value::symbol("bold"));
    table.put_property(10, 15, Value::symbol("face"), Value::symbol("italic"));
    table.put_property(20, 25, Value::symbol("face"), Value::symbol("underline"));

    assert!(table.get_property(3, Value::symbol("face")).is_some());
    assert!(table.get_property(7, Value::symbol("face")).is_none());
    assert!(table.get_property(12, Value::symbol("face")).is_some());
    assert!(table.get_property(17, Value::symbol("face")).is_none());
    assert!(table.get_property(22, Value::symbol("face")).is_some());
}
