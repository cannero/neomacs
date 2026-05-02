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

// -----------------------------------------------------------------------
// Dired-style multi-step operations (simulating insert-directory decode loop)
// -----------------------------------------------------------------------

/// Put `dired-filename` property on non-contiguous ranges, then simulate
/// the decode-coding-region loop that deletes and reinserts text in each
/// chunk.  This catches the bug where `next_property_change` returns None
/// after buffer modifications shift the interval runs.
#[test]
fn dired_decode_loop_property_survival() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    let prop = Value::symbol("dired-filename");
    let val = Value::T;

    // Simulate ls --dired output with 4 filenames at non-contiguous ranges.
    // Buffer layout: [header...][file1][spaces...][file2][spaces...][file3][spaces...][file4]
    table.put_property(58, 64, prop, val); // file1 at [58, 64)
    table.put_property(80, 86, prop, val); // file2 at [80, 86)
    table.put_property(102, 108, prop, val); // file3 at [102, 108)
    table.put_property(124, 130, prop, val); // file4 at [124, 130)

    // Verify initial next_property_change works for all ranges
    assert_eq!(table.next_property_change(0), Some(58), "should find file1");
    assert_eq!(table.next_property_change(58), Some(64), "should find end of file1");
    assert_eq!(table.next_property_change(64), Some(80), "should find file2 from gap");
    assert_eq!(table.next_property_change(80), Some(86), "should find end of file2");
    assert_eq!(table.next_property_change(86), Some(102), "should find file3 from gap");
    assert_eq!(table.next_property_change(102), Some(108), "should find end of file3");
    assert_eq!(table.next_property_change(108), Some(124), "should find file4 from gap");
    assert_eq!(table.next_property_change(124), Some(130), "should find end of file4");
    assert_eq!(table.next_property_change(130), None, "should be done after file4");

    // Now simulate the decode loop. In insert-directory, the decode loop:
    // 1. Starts at point-min (char position 0)
    // 2. Finds next dired-filename change via next_property_change
    // 3. decode-coding-region from current to next position
    //    (which does: delete old text + insert new text)
    // 4. If the chunk had dired-filename, re-put the property
    // 5. Repeat until eobp

    // Simulate first chunk: pos=0 to 58 (header text, no dired-filename)
    // decode-coding-region on [0, 58) — text may shrink or expand
    // For this test, simulate that the decoded text is 2 chars shorter
    let mut old_len = 58;
    let mut new_len = 56;
    table.adjust_for_delete(0, old_len);
    table.adjust_for_insert(0, new_len);
    // After this, all property positions should shift by (new_len - old_len) = -2
    // file1 was at [58,64), now at [56,62)
    // file2 was at [80,86), now at [78,84)
    // etc.

    assert_eq!(table.next_property_change(0), Some(56), "file1 should shift by -2 after first decode");
    assert_eq!(table.next_property_change(56), Some(62), "end of file1");

    // Now simulate second chunk: pos=56 to 62 (file1, has dired-filename)
    // decode-coding-region with coding-no-eol — text may change length
    old_len = 6; // 62 - 56
    new_len = 4; // file1 decoded (UTF-8 multibyte chars become single chars, text shrinks)
    table.adjust_for_delete(56, 56 + old_len); // delete old file1 text
    table.adjust_for_insert(56, new_len); // insert decoded text
    // file1 is now at [56, 60) — shift of -2 from previous
    // file2 was at [78,84), now shifts by (4-6) = -2 → [76, 82)
    // Re-put dired-filename on the decoded chunk
    table.put_property(56, 60, prop, val);

    assert_eq!(table.next_property_change(0), Some(56), "file1 still at 56");
    assert_eq!(table.next_property_change(56), Some(60), "end of decoded file1");
    assert_eq!(table.next_property_change(60), Some(76), "file2 shifted correctly");
    assert_eq!(table.next_property_change(76), Some(82), "end of file2");

    // Third chunk: gaps between file2 and file3
    // This tests that iterate-through-gaps correctly finds the next property
    assert_eq!(table.next_property_change(82), Some(98), "file3 should be after gap");
    assert_eq!(table.next_property_change(98), Some(104), "end of file3");
    assert_eq!(table.next_property_change(104), Some(120), "file4 should be after gap");
    assert_eq!(table.next_property_change(120), Some(126), "end of file4");
    assert_eq!(table.next_property_change(126), None, "no more properties");
}

/// Simulate the exact sequence from insert-directory-clean:
/// put properties, then delete lines from the buffer.
#[test]
fn insert_directory_clean_then_delete_lines() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    let prop = Value::symbol("dired-filename");
    let val = Value::T;

    // Put properties at non-contiguous ranges (filenames in ls output)
    table.put_property(58, 64, prop, val);
    table.put_property(80, 86, prop, val);
    table.put_property(102, 108, prop, val);

    // Simulate delete-region of //DIRED// lines at the end of buffer
    // In insert-directory-clean, lines are deleted from the dired section
    // (typically near the end of the ls output)
    // Delete region [130, 160) which is AFTER all the filename properties
    table.adjust_for_delete(130, 160);

    // Properties before the deleted region should be unaffected
    assert_eq!(table.next_property_change(0), Some(58));
    assert_eq!(table.next_property_change(58), Some(64));
    assert_eq!(table.next_property_change(64), Some(80));
    assert_eq!(table.next_property_change(80), Some(86));
    assert_eq!(table.next_property_change(86), Some(102));
    assert_eq!(table.next_property_change(102), Some(108));
    assert_eq!(table.next_property_change(108), None);

    // Now simulate decoding a chunk that is BEFORE all properties
    // This is the first iteration of the decode loop
    table.adjust_for_delete(0, 58);
    table.adjust_for_insert(0, 55); // decoded text is shorter

    // All properties should shift by -3
    assert_eq!(table.next_property_change(0), Some(55), "file1 shifted to 55");
    assert_eq!(table.next_property_change(55), Some(61));
    assert_eq!(table.next_property_change(61), Some(77), "file2 shifted to 77");
    assert_eq!(table.next_property_change(77), Some(83));
    assert_eq!(table.next_property_change(83), Some(99), "file3 shifted to 99");
    assert_eq!(table.next_property_change(99), Some(105));
    assert_eq!(table.next_property_change(105), None);
}

/// Regression test: adjust_for_delete can produce runs where start >= end,
/// which then causes next_property_change to loop infinitely or miss intervals.
#[test]
fn adjust_delete_produces_no_negative_len_runs() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    let prop = Value::symbol("dired-filename");
    let val = Value::T;

    // Put properties
    table.put_property(10, 20, prop, val);
    table.put_property(30, 40, prop, val);

    // Delete a region that partially overlaps the end of the first interval
    table.adjust_for_delete(15, 25);

    // First interval should be truncated to [10, 15)
    assert_eq!(table.next_property_change(0), Some(10));
    assert_eq!(table.next_property_change(10), Some(15), "first interval truncated at 15");

    // Second interval should shift left by 10 (25-15=10)
    // Was [30, 40), now [20, 30)
    assert_eq!(table.next_property_change(15), Some(20), "second interval at 20");
    assert_eq!(table.next_property_change(20), Some(30), "second interval ends at 30");
    assert_eq!(table.next_property_change(30), None);

    // Verify no runs have start >= end
    for run in &table.runs {
        assert!(run.start < run.end, "run [{},{}) has start >= end", run.start, run.end);
    }
    // There should be exactly 2 non-empty runs (no empty gap runs needed
    // since adjust_for_delete doesn't create them; next_property_change
    // handles gaps naturally by skipping to the next non-empty run)
    let non_empty: Vec<_> = table.runs.iter().filter(|r| !r.is_empty_plist()).collect();
    assert_eq!(non_empty.len(), 2);
}

/// Exact simulation of the decode loop in insert-directory.
/// This test reproduces the exact sequence of operations that should happen
/// during the decode loop.  After putting dired-filename on 4 non-contiguous
/// filename ranges, we simulate decode-coding-region on each chunk.  The key
/// assertion: get_property at the start of a GAP chunk returns None (nil),
/// so the Lisp (if val ...) guard should prevent put-text-property on gaps.
#[test]
fn decode_loop_get_property_at_gap_boundaries() {
    crate::test_utils::init_test_tracing();
    let mut table = TextPropertyTable::new();
    let prop = Value::symbol("dired-filename");
    let val = Value::T;

    // Initial state: 4 filename properties at non-contiguous ranges.
    // These match the pattern from the trace: [57..58), [105..107), etc.
    table.put_property(57, 58, prop, val);   // file1: 1 char
    table.put_property(105, 107, prop, val); // file2: 2 chars
    table.put_property(154, 163, prop, val); // file3: 9 chars
    table.put_property(210, 218, prop, val); // file4: 8 chars

    // Verify initial state
    assert_eq!(table.next_property_change(0), Some(57));
    assert!(table.get_property(57, prop).is_some(), "pos 57 should have df");
    assert!(table.get_property(58, prop).is_none(), "pos 58 should NOT have df (end of file1)");

    // === Iteration 1: decode header [0, 57) ===
    // val = get_property(0) = nil → do NOT re-put
    let mut old_len = 57;
    let mut new_len = 57; // decoded text same length
    table.adjust_for_delete(0, old_len);
    table.adjust_for_insert(0, new_len);
    // No put-text-property because val was nil

    // Verify: positions unchanged (same length insert)
    assert_eq!(table.next_property_change(0), Some(57));
    assert!(table.get_property(57, prop).is_some());
    assert!(table.get_property(58, prop).is_none(), "pos 58 should still NOT have df after iter1");

    // === Iteration 2: decode file1 [57, 58) ===
    // val = get_property(57) = t → re-put after decode
    old_len = 1; // 58 - 57
    new_len = 1; // decoded text same length
    table.adjust_for_delete(57, 57 + old_len);
    table.adjust_for_insert(57, new_len);
    table.put_property(57, 58, prop, val);  // re-put (val was t)

    // Verify: file1 property preserved, pos 58 still gap
    assert!(table.get_property(57, prop).is_some());
    assert!(table.get_property(58, prop).is_none(),
            "CRITICAL: pos 58 must be nil - next iteration captures val here");
    assert_eq!(table.next_property_change(58), Some(105),
               "next change from pos 58 should be file2 at 105");

    // === Iteration 3: decode GAP [58, 105) ===
    // val = get_property(58) = nil → should NOT re-put
    // BUT the trace shows put [58..105) IS happening!
    // This test checks whether get_property(58) correctly returns nil.
    assert!(table.get_property(58, prop).is_none(),
            "BUG CONFIRMATION: if this fails, get_property returns non-nil at pos 58");

    old_len = 47; // 105 - 58
    new_len = 47; // same length decode
    table.adjust_for_delete(58, 58 + old_len);
    // After delete, file2 [105..107) shifts to [58..60), merges with [57..58) → [57..60)
    // This is correct behavior — the gap between file1 and file2 is eliminated by delete.
    // The subsequent insert re-creates the gap.
    table.adjust_for_insert(58, new_len);

    // After insert, file2 shifts back. Check that pos 58 is still nil.
    assert!(table.get_property(58, prop).is_none(),
            "CRITICAL: pos 58 must be nil after decode of gap - put should NOT be called");

    // Now simulate what the BUG does: put dired-filename on [58, 105) even though val was nil
    // (this is what we observe in the trace)
    // If we put here, the two adjacent df ranges merge:
    table.put_property(58, 58 + new_len, prop, val);
    // After this erroneous put, [57..58) and [58..105) merge → [57..107)
    // This is the cascading merge bug!
    assert_eq!(table.next_property_change(58), Some(107),
               "after erroneous put on gap, file1 and file2 merge: next change at 107");

    // Verify that the merge happened
    let snapshot = table.intervals_snapshot();
    assert_eq!(snapshot.len(), 3, "merged: 4 ranges → 3 after gap put");
    // Note: if get_property(58) correctly returns nil, this put would NEVER happen
    // because the Lisp (if val ...) guard prevents it
}
