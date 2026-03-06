//! Pure Rust undo system.
//!
//! Implements the core of Emacs's undo mechanism (cf. `undo.c`).
//! The module is self-contained: it records changes, computes the
//! inverse actions needed to undo one group at a time, and manages
//! memory by truncating old history when a configurable byte budget
//! is exceeded.
//!
//! # Redo
//!
//! Redo is achieved the Emacs way: when the caller applies the
//! [`UndoAction`]s returned by [`UndoList::undo_one_group`], it
//! records those applications as *new* changes. Undoing those new
//! changes effectively "redoes" the original edit.
//!
//! # Size management
//!
//! Every record that contains text contributes its byte length to
//! [`UndoList::current_size`]. [`UndoList::truncate`] drops the
//! oldest records (those at the *front* of the internal `Vec`) until
//! the budget is satisfied. Boundary markers have zero cost.

use std::mem;

// ---------------------------------------------------------------------------
// Record types
// ---------------------------------------------------------------------------

/// A single entry in the undo list.
///
/// Each variant corresponds to one kind of buffer mutation that Emacs
/// records for later reversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoRecord {
    /// Text was inserted in the range `[start, end)`.
    /// To undo: delete that range.
    Insert { start: usize, end: usize },

    /// Text was deleted from `position`.
    /// To undo: re-insert `text` at `position`.
    /// `was_before_point` records whether the deleted text was before
    /// point, which matters for the correct cursor placement on undo.
    Delete {
        position: usize,
        text: String,
        was_before_point: bool,
    },

    /// One or more text properties were changed over `[position, position+length)`.
    /// `old_props` stores the property values *before* the change so
    /// they can be restored on undo.
    PropertyChange {
        position: usize,
        length: usize,
        old_props: Vec<(String, String)>,
    },

    /// The cursor (point) was at `position` before the next change.
    CursorMove { position: usize },

    /// Boundary separating two undo groups (one user-visible action).
    Boundary,

    /// The buffer was in an unmodified state at this point.
    Unmodified,

    /// Selective-delete marker (for region-restricted undo).
    SelectiveDelete { position: usize, text: String },
}

impl UndoRecord {
    /// Estimated byte cost of this record for size-management purposes.
    fn size_cost(&self) -> usize {
        match self {
            UndoRecord::Insert { .. } => mem::size_of::<usize>() * 2,
            UndoRecord::Delete { text, .. } => text.len(),
            UndoRecord::PropertyChange { old_props, .. } => old_props
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>(),
            UndoRecord::SelectiveDelete { text, .. } => text.len(),
            // Markers, boundaries, cursor moves — essentially free.
            UndoRecord::CursorMove { .. } | UndoRecord::Boundary | UndoRecord::Unmodified => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Actions returned by undo
// ---------------------------------------------------------------------------

/// An action the caller must apply to the buffer in order to effect
/// one step of an undo operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoAction {
    /// Insert `text` at `position`.
    Insert { position: usize, text: String },

    /// Delete the range `[start, end)`.
    Delete { start: usize, end: usize },

    /// Move the cursor to `position`.
    MoveCursor { position: usize },

    /// Restore text properties over `[position, position+length)`.
    SetProperties {
        position: usize,
        length: usize,
        props: Vec<(String, String)>,
    },
}

// ---------------------------------------------------------------------------
// The undo list
// ---------------------------------------------------------------------------

/// The undo history for a single buffer.
///
/// Records are appended to the *end* of an internal `Vec`.  The most
/// recent record is therefore `records.last()`.  Truncation removes
/// from the *front* (oldest records first).
#[derive(Debug, Clone)]
pub struct UndoList {
    /// Chronologically ordered undo records (oldest first).
    records: Vec<UndoRecord>,

    /// Maximum number of bytes to keep in undo history.
    max_size: usize,

    /// Running total of the byte cost of all records.
    current_size: usize,

    /// Whether undo recording is currently enabled.
    enabled: bool,

    /// How far into `records` we have already undone (index of the
    /// next record to consider when the caller calls `undo_one_group`
    /// repeatedly).  Reset to `records.len()` whenever a non-undo
    /// mutation is recorded.
    undo_pointer: usize,
}

impl UndoList {
    // -- Construction -------------------------------------------------------

    /// Create a new, empty undo list with a byte budget of `max_size`.
    pub fn new(max_size: usize) -> Self {
        Self {
            records: Vec::new(),
            max_size,
            current_size: 0,
            enabled: true,
            undo_pointer: 0,
        }
    }

    // -- Recording ----------------------------------------------------------

    /// Record that text was inserted in `[start, end)`.
    pub fn record_insert(&mut self, start: usize, end: usize) {
        if !self.enabled {
            return;
        }
        self.push(UndoRecord::Insert { start, end });
    }

    /// Record that `text` was deleted from `position`.
    ///
    /// `before_point` indicates whether the deleted text was before
    /// the cursor — this determines cursor placement on undo.
    pub fn record_delete(&mut self, position: usize, text: String, before_point: bool) {
        if !self.enabled {
            return;
        }
        self.push(UndoRecord::Delete {
            position,
            text,
            was_before_point: before_point,
        });
    }

    /// Record a text-property change over `[pos, pos+len)`.
    pub fn record_property_change(
        &mut self,
        pos: usize,
        len: usize,
        old_props: Vec<(String, String)>,
    ) {
        if !self.enabled {
            return;
        }
        self.push(UndoRecord::PropertyChange {
            position: pos,
            length: len,
            old_props,
        });
    }

    /// Record that the cursor was at `pos` before the next change.
    pub fn record_cursor_move(&mut self, pos: usize) {
        if !self.enabled {
            return;
        }
        self.push(UndoRecord::CursorMove { position: pos });
    }

    /// Insert a boundary that separates two undo groups.
    pub fn add_boundary(&mut self) {
        if !self.enabled {
            return;
        }
        // Avoid consecutive boundaries.
        if let Some(UndoRecord::Boundary) = self.records.last() {
            return;
        }
        // Boundaries have zero size cost — push directly.
        self.records.push(UndoRecord::Boundary);
        self.reset_undo_pointer();
    }

    /// Mark the buffer as unmodified at this point.
    pub fn mark_unmodified(&mut self) {
        if !self.enabled {
            return;
        }
        self.push(UndoRecord::Unmodified);
    }

    // -- Amalgamation -------------------------------------------------------

    /// Extend the most recent `Insert` record's `end` to `new_end`.
    ///
    /// This is used to coalesce consecutive single-character
    /// insertions (e.g. typing) into a single record so that undo
    /// reverses the whole run at once.
    ///
    /// Returns `true` if amalgamation succeeded (the last non-boundary
    /// record was an `Insert` whose `end` matched the expected
    /// position), `false` otherwise.
    pub fn amalgamate_last_insert(&mut self, new_end: usize) -> bool {
        // Walk backwards past boundaries to find the most recent
        // substantive record.
        for rec in self.records.iter_mut().rev() {
            match rec {
                UndoRecord::Boundary => continue,
                UndoRecord::Insert { end, .. } => {
                    // Only amalgamate if the new end is contiguous.
                    if new_end > *end {
                        // Adjust size: Insert size is constant (two usizes),
                        // so no current_size change needed.
                        *end = new_end;
                        return true;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        false
    }

    // -- Undo execution -----------------------------------------------------

    /// Compute the [`UndoAction`]s required to undo one group.
    ///
    /// A "group" is delimited by [`UndoRecord::Boundary`] markers.
    /// The method walks backwards from the current undo pointer,
    /// skipping an initial boundary (if present), and collects
    /// actions until the next boundary or the beginning of the list.
    ///
    /// Returns `None` when there is nothing left to undo.
    ///
    /// `current_pos` is the cursor position before the undo; it is
    /// used for informational purposes (not yet used in the current
    /// implementation, but reserved for future cursor-placement
    /// logic).
    pub fn undo_one_group(&mut self, _current_pos: usize) -> Option<Vec<UndoAction>> {
        if self.undo_pointer == 0 {
            return None;
        }

        let mut actions = Vec::new();
        let mut idx = self.undo_pointer;

        // Skip a trailing boundary (the one that closes the group).
        if idx > 0 {
            if let Some(UndoRecord::Boundary) = self.records.get(idx - 1) {
                idx -= 1;
            }
        }

        // Collect actions until we hit another boundary or the start.
        while idx > 0 {
            idx -= 1;
            match &self.records[idx] {
                UndoRecord::Boundary => {
                    // End of this group.
                    break;
                }
                UndoRecord::Insert { start, end } => {
                    actions.push(UndoAction::Delete {
                        start: *start,
                        end: *end,
                    });
                }
                UndoRecord::Delete { position, text, .. } => {
                    actions.push(UndoAction::Insert {
                        position: *position,
                        text: text.clone(),
                    });
                }
                UndoRecord::PropertyChange {
                    position,
                    length,
                    old_props,
                } => {
                    actions.push(UndoAction::SetProperties {
                        position: *position,
                        length: *length,
                        props: old_props.clone(),
                    });
                }
                UndoRecord::CursorMove { position } => {
                    actions.push(UndoAction::MoveCursor {
                        position: *position,
                    });
                }
                UndoRecord::SelectiveDelete { position, text, .. } => {
                    actions.push(UndoAction::Insert {
                        position: *position,
                        text: text.clone(),
                    });
                }
                UndoRecord::Unmodified => {
                    // No buffer action, but the caller may want to
                    // mark the buffer unmodified.  We simply skip it
                    // in the action list.
                }
            }
        }

        if actions.is_empty() && idx == 0 && self.undo_pointer == idx {
            return None;
        }

        self.undo_pointer = idx;
        Some(actions)
    }

    /// Reset the undo pointer so the next `undo_one_group` starts
    /// from the most recent record.  Call this after recording new
    /// (non-undo) mutations.
    fn reset_undo_pointer(&mut self) {
        self.undo_pointer = self.records.len();
    }

    // -- Size management ----------------------------------------------------

    /// Remove the oldest records until `current_size <= max_size`.
    ///
    /// Records are removed from the *front* of the list (oldest
    /// first).  If a boundary sits at the new front, it is also
    /// removed so that the list doesn't start with a stale boundary.
    pub fn truncate(&mut self) {
        while self.current_size > self.max_size && !self.records.is_empty() {
            let cost = self.records[0].size_cost();
            self.records.remove(0);
            self.current_size = self.current_size.saturating_sub(cost);
            // Keep undo_pointer valid.
            if self.undo_pointer > 0 {
                self.undo_pointer -= 1;
            }
        }

        // Remove a leading boundary (it serves no purpose).
        while let Some(UndoRecord::Boundary) = self.records.first() {
            self.records.remove(0);
            if self.undo_pointer > 0 {
                self.undo_pointer -= 1;
            }
        }
    }

    /// Clear all undo history.
    pub fn clear(&mut self) {
        self.records.clear();
        self.current_size = 0;
        self.undo_pointer = 0;
    }

    // -- Queries ------------------------------------------------------------

    /// Whether the undo list contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Total number of records (including boundaries).
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether there is at least one undoable group.
    pub fn can_undo(&self) -> bool {
        self.undo_pointer > 0
            && self.records[..self.undo_pointer]
                .iter()
                .any(|r| !matches!(r, UndoRecord::Boundary))
    }

    /// Whether undo recording is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable undo recording.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Current estimated byte cost of the undo history.
    pub fn current_size(&self) -> usize {
        self.current_size
    }

    /// Configured maximum byte budget.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Set a new maximum byte budget.
    pub fn set_max_size(&mut self, max_size: usize) {
        self.max_size = max_size;
    }

    /// Read-only access to the underlying records (for inspection /
    /// debugging).
    pub fn records(&self) -> &[UndoRecord] {
        &self.records
    }

    // -- Internal -----------------------------------------------------------

    /// Append a record, update size accounting, and reset the undo
    /// pointer so subsequent undos start from the new end.
    fn push(&mut self, record: UndoRecord) {
        self.current_size += record.size_cost();
        self.records.push(record);
        self.reset_undo_pointer();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers ---------------------------------------------------------------

    fn make_list() -> UndoList {
        UndoList::new(10_000)
    }

    // 1. Basic insert record + undo -----------------------------------------

    #[test]
    fn test_record_and_undo_insert() {
        let mut ul = make_list();
        ul.record_insert(0, 5);
        ul.add_boundary();

        let actions = ul.undo_one_group(5).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], UndoAction::Delete { start: 0, end: 5 });
    }

    // 2. Basic delete record + undo -----------------------------------------

    #[test]
    fn test_record_and_undo_delete() {
        let mut ul = make_list();
        ul.record_delete(3, "hello".into(), true);
        ul.add_boundary();

        let actions = ul.undo_one_group(3).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            UndoAction::Insert {
                position: 3,
                text: "hello".into()
            }
        );
    }

    // 3. Cursor move record + undo ------------------------------------------

    #[test]
    fn test_record_and_undo_cursor_move() {
        let mut ul = make_list();
        ul.record_cursor_move(42);
        ul.add_boundary();

        let actions = ul.undo_one_group(0).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], UndoAction::MoveCursor { position: 42 });
    }

    // 4. Property change record + undo --------------------------------------

    #[test]
    fn test_record_and_undo_property_change() {
        let mut ul = make_list();
        let props = vec![("face".into(), "bold".into())];
        ul.record_property_change(10, 5, props.clone());
        ul.add_boundary();

        let actions = ul.undo_one_group(0).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            UndoAction::SetProperties {
                position: 10,
                length: 5,
                props,
            }
        );
    }

    // 5. Boundary separates groups ------------------------------------------

    #[test]
    fn test_boundary_separates_groups() {
        let mut ul = make_list();
        ul.record_insert(0, 3);
        ul.add_boundary();
        ul.record_insert(3, 6);
        ul.add_boundary();

        // First undo: the second insertion.
        let a1 = ul.undo_one_group(6).unwrap();
        assert_eq!(a1.len(), 1);
        assert_eq!(a1[0], UndoAction::Delete { start: 3, end: 6 });

        // Second undo: the first insertion.
        let a2 = ul.undo_one_group(3).unwrap();
        assert_eq!(a2.len(), 1);
        assert_eq!(a2[0], UndoAction::Delete { start: 0, end: 3 });
    }

    // 6. Multiple records in one group --------------------------------------

    #[test]
    fn test_multiple_records_in_one_group() {
        let mut ul = make_list();
        ul.record_cursor_move(0);
        ul.record_insert(0, 5);
        ul.record_delete(5, "xyz".into(), false);
        ul.add_boundary();

        let actions = ul.undo_one_group(5).unwrap();
        // Reverse order within the group (most recent first).
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], UndoAction::Insert { .. }));
        assert!(matches!(actions[1], UndoAction::Delete { .. }));
        assert!(matches!(actions[2], UndoAction::MoveCursor { .. }));
    }

    // 7. Undo returns None when empty ---------------------------------------

    #[test]
    fn test_undo_empty_returns_none() {
        let mut ul = make_list();
        assert!(ul.undo_one_group(0).is_none());
    }

    // 8. Undo returns None after everything is undone -----------------------

    #[test]
    fn test_undo_exhausted_returns_none() {
        let mut ul = make_list();
        ul.record_insert(0, 1);
        ul.add_boundary();

        ul.undo_one_group(1); // undo the only group
        assert!(ul.undo_one_group(0).is_none());
    }

    // 9. clear() resets everything ------------------------------------------

    #[test]
    fn test_clear() {
        let mut ul = make_list();
        ul.record_insert(0, 100);
        ul.record_delete(0, "big string".into(), true);
        ul.add_boundary();

        assert!(!ul.is_empty());
        ul.clear();

        assert!(ul.is_empty());
        assert_eq!(ul.len(), 0);
        assert_eq!(ul.current_size(), 0);
        assert!(!ul.can_undo());
    }

    // 10. is_empty / len / can_undo -----------------------------------------

    #[test]
    fn test_queries() {
        let mut ul = make_list();
        assert!(ul.is_empty());
        assert_eq!(ul.len(), 0);
        assert!(!ul.can_undo());

        ul.record_insert(0, 1);
        assert!(!ul.is_empty());
        assert_eq!(ul.len(), 1);
        assert!(ul.can_undo());

        ul.add_boundary();
        // Boundary alone should not count as undoable, but we still
        // have the insert before it.
        assert!(ul.can_undo());
        assert_eq!(ul.len(), 2);
    }

    // 11. Truncation trims oldest records -----------------------------------

    #[test]
    fn test_truncate_removes_oldest() {
        let mut ul = UndoList::new(10); // very small budget
        ul.record_delete(0, "aaaaaaaaaa".into(), true); // 10 bytes
        ul.add_boundary();
        ul.record_delete(0, "bbbbbbbbbb".into(), true); // 10 bytes
        ul.add_boundary();

        assert_eq!(ul.current_size(), 20);
        ul.truncate();

        // After truncation we should be at or below 10 bytes.
        assert!(ul.current_size() <= ul.max_size());
        // At least the second delete should survive.
        assert!(!ul.is_empty());
    }

    // 12. Truncation with zero budget clears everything ---------------------

    #[test]
    fn test_truncate_zero_budget() {
        let mut ul = UndoList::new(0);
        ul.record_delete(0, "hello".into(), true);
        ul.truncate();
        // All text-carrying records should be gone.
        assert_eq!(ul.current_size(), 0);
    }

    // 13. Amalgamation extends last insert ----------------------------------

    #[test]
    fn test_amalgamate_last_insert() {
        let mut ul = make_list();
        ul.record_insert(0, 1);
        assert!(ul.amalgamate_last_insert(2));
        assert!(ul.amalgamate_last_insert(3));

        ul.add_boundary();
        let actions = ul.undo_one_group(3).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], UndoAction::Delete { start: 0, end: 3 });
    }

    // 14. Amalgamation fails when last record is not an insert ---------------

    #[test]
    fn test_amalgamate_fails_on_non_insert() {
        let mut ul = make_list();
        ul.record_delete(0, "x".into(), true);
        assert!(!ul.amalgamate_last_insert(1));
    }

    // 15. Amalgamation skips boundaries -------------------------------------

    #[test]
    fn test_amalgamate_skips_boundaries() {
        let mut ul = make_list();
        ul.record_insert(0, 1);
        ul.add_boundary();
        // The boundary is between; amalgamation should still find
        // the insert behind the boundary.
        assert!(ul.amalgamate_last_insert(2));
    }

    // 16. Disabled recording suppresses records -----------------------------

    #[test]
    fn test_disabled_recording() {
        let mut ul = make_list();
        ul.set_enabled(false);
        ul.record_insert(0, 5);
        ul.record_delete(0, "hello".into(), true);
        ul.record_cursor_move(10);
        ul.add_boundary();

        assert!(ul.is_empty());
        assert!(!ul.can_undo());
    }

    // 17. Re-enable recording after disable ---------------------------------

    #[test]
    fn test_reenable_recording() {
        let mut ul = make_list();
        ul.set_enabled(false);
        ul.record_insert(0, 5);
        ul.set_enabled(true);
        ul.record_insert(0, 5);

        assert_eq!(ul.len(), 1);
    }

    // 18. mark_unmodified appears in list -----------------------------------

    #[test]
    fn test_mark_unmodified() {
        let mut ul = make_list();
        ul.record_insert(0, 5);
        ul.mark_unmodified();
        ul.add_boundary();

        // Undo the group — two records (insert + unmodified), but
        // unmodified produces no action.
        let actions = ul.undo_one_group(5).unwrap();
        assert_eq!(actions.len(), 1); // only the delete
        assert!(matches!(actions[0], UndoAction::Delete { .. }));
    }

    // 19. Redo via recording undo actions -----------------------------------

    #[test]
    fn test_redo_via_recording() {
        let mut ul = make_list();

        // Original edit: insert "abc" at 0..3.
        ul.record_insert(0, 3);
        ul.add_boundary();

        // Undo it.
        let undo_actions = ul.undo_one_group(3).unwrap();
        assert_eq!(undo_actions.len(), 1);
        assert_eq!(undo_actions[0], UndoAction::Delete { start: 0, end: 3 });

        // The caller applies the delete and records it as a new
        // change (this is how Emacs redo works):
        ul.record_delete(0, "abc".into(), true);
        ul.add_boundary();

        // Now "redo" by undoing the undo-delete.
        let redo_actions = ul.undo_one_group(0).unwrap();
        assert_eq!(redo_actions.len(), 1);
        assert_eq!(
            redo_actions[0],
            UndoAction::Insert {
                position: 0,
                text: "abc".into(),
            }
        );
    }

    // 20. Selective delete record + undo ------------------------------------

    #[test]
    fn test_selective_delete_undo() {
        let mut ul = make_list();
        ul.push(UndoRecord::SelectiveDelete {
            position: 5,
            text: "sel".into(),
        });
        ul.add_boundary();

        let actions = ul.undo_one_group(5).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            UndoAction::Insert {
                position: 5,
                text: "sel".into(),
            }
        );
    }

    // 21. Consecutive boundaries are collapsed ------------------------------

    #[test]
    fn test_consecutive_boundaries_collapsed() {
        let mut ul = make_list();
        ul.add_boundary();
        ul.add_boundary();
        ul.add_boundary();

        // Only one boundary should be stored.
        assert_eq!(ul.len(), 1);
    }

    // 22. Size accounting is correct ----------------------------------------

    #[test]
    fn test_size_accounting() {
        let mut ul = make_list();
        assert_eq!(ul.current_size(), 0);

        ul.record_delete(0, "hello".into(), true); // 5 bytes
        assert_eq!(ul.current_size(), 5);

        ul.record_delete(0, "world!".into(), true); // 6 bytes
        assert_eq!(ul.current_size(), 11);

        ul.add_boundary(); // 0 bytes
        assert_eq!(ul.current_size(), 11);
    }

    // 23. Truncation preserves undo_pointer validity ------------------------

    #[test]
    fn test_truncate_preserves_undo_pointer() {
        let mut ul = UndoList::new(5);
        ul.record_delete(0, "aaaa".into(), true); // 4 bytes
        ul.add_boundary();
        ul.record_delete(0, "bbb".into(), true); // 3 bytes
        ul.add_boundary();

        // Total 7 > 5; truncation should drop the first delete.
        ul.truncate();
        assert!(ul.current_size() <= 5);

        // We should still be able to undo the surviving group.
        assert!(ul.can_undo());
        let actions = ul.undo_one_group(0).unwrap();
        assert!(!actions.is_empty());
    }

    // 24. Large multi-group sequence ----------------------------------------

    #[test]
    fn test_large_multi_group() {
        let mut ul = make_list();
        for i in 0..50 {
            ul.record_cursor_move(i);
            ul.record_insert(i, i + 1);
            ul.add_boundary();
        }

        // Undo all 50 groups.
        let mut count = 0;
        while ul.undo_one_group(0).is_some() {
            count += 1;
        }
        assert_eq!(count, 50);
    }

    // 25. Undo group without explicit trailing boundary ----------------------

    #[test]
    fn test_undo_without_trailing_boundary() {
        let mut ul = make_list();
        ul.record_insert(0, 5);
        // No boundary added — undo should still work (treats
        // everything from pointer to start as one group).

        let actions = ul.undo_one_group(5).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], UndoAction::Delete { start: 0, end: 5 });
    }

    // 26. Property change size accounting -----------------------------------

    #[test]
    fn test_property_change_size() {
        let mut ul = make_list();
        let props = vec![
            ("face".into(), "bold".into()), // 4 + 4 = 8
            ("color".into(), "red".into()), // 5 + 3 = 8
        ];
        ul.record_property_change(0, 10, props);
        assert_eq!(ul.current_size(), 16);
    }

    // 27. Insert record size accounting (constant) --------------------------

    #[test]
    fn test_insert_size_accounting() {
        let mut ul = make_list();
        ul.record_insert(0, 1000);
        // Insert cost is 2 * size_of::<usize>(), not proportional
        // to the range.
        assert_eq!(ul.current_size(), std::mem::size_of::<usize>() * 2);
    }

    // 28. can_undo is false for boundary-only list --------------------------

    #[test]
    fn test_can_undo_boundary_only() {
        let mut ul = make_list();
        ul.records.push(UndoRecord::Boundary);
        ul.undo_pointer = ul.records.len();
        assert!(!ul.can_undo());
    }

    // 29. max_size / set_max_size -------------------------------------------

    #[test]
    fn test_max_size_accessors() {
        let mut ul = UndoList::new(500);
        assert_eq!(ul.max_size(), 500);
        ul.set_max_size(1000);
        assert_eq!(ul.max_size(), 1000);
    }

    // 30. Interleaved undo and new edits ------------------------------------

    #[test]
    fn test_interleaved_undo_and_edits() {
        let mut ul = make_list();

        // Group 1: insert "ab".
        ul.record_insert(0, 2);
        ul.add_boundary();

        // Group 2: insert "cd".
        ul.record_insert(2, 4);
        ul.add_boundary();

        // Undo group 2.
        let a = ul.undo_one_group(4).unwrap();
        assert_eq!(a[0], UndoAction::Delete { start: 2, end: 4 });

        // New edit after partial undo: insert "ef".
        // This resets undo_pointer to end, so ALL prior records
        // (including the already-undone group 2) become visible again.
        ul.record_insert(2, 4);
        ul.add_boundary();

        // Undo the new "ef" group (most recent).
        let b = ul.undo_one_group(4).unwrap();
        assert_eq!(b[0], UndoAction::Delete { start: 2, end: 4 });

        // The original group 2 is still in the list; undo it again.
        let c = ul.undo_one_group(2).unwrap();
        assert_eq!(c[0], UndoAction::Delete { start: 2, end: 4 });

        // Now undo group 1.
        let d = ul.undo_one_group(2).unwrap();
        assert_eq!(d[0], UndoAction::Delete { start: 0, end: 2 });
    }
}
