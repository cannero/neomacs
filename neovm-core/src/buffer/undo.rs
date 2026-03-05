//! Undo/redo system for buffers.
//!
//! Implements Emacs-style undo: a linear list of undo records with
//! explicit boundaries separating user-visible undo steps.
//!
//! Record types:
//! - `Insert(pos, len)` — text was inserted; to undo, delete it
//! - `Delete(pos, text)` — text was deleted; to undo, re-insert it
//! - `PropertyChange(pos, len, old_props)` — text properties changed
//! - `Boundary` — separates undo groups
//! - `CursorMove(old_pos)` — cursor was at `old_pos` before the edit

use crate::emacs_core::value::Value;
use crate::gc::GcTrace;
use std::collections::HashMap;

/// A single undo record.
#[derive(Clone, Debug)]
pub enum UndoRecord {
    /// Text was inserted at `pos` with byte length `len`.
    /// Undo action: delete `[pos, pos+len)`.
    Insert { pos: usize, len: usize },

    /// Text was deleted: `text` was removed from `pos`.
    /// Undo action: insert `text` at `pos`.
    Delete { pos: usize, text: String },

    /// Text properties were changed on range `[pos, pos+len)`.
    /// Stores old property values for restoration.
    PropertyChange {
        pos: usize,
        len: usize,
        old_props: HashMap<String, Value>,
    },

    /// Cursor position before the next edit.
    CursorMove { pos: usize },

    /// Boundary separating undo groups.
    Boundary,
}

/// Undo list for a single buffer.
#[derive(Clone, Debug)]
pub struct UndoList {
    /// Stack of undo records (most recent at the end).
    records: Vec<UndoRecord>,
    /// Maximum number of records before truncation (0 = unlimited).
    limit: usize,
    /// Whether recording is enabled.
    enabled: bool,
    /// Whether we are currently inside an undo group (no boundary yet).
    in_group: bool,
    /// True when `primitive-undo` is executing (suppress re-recording).
    pub undoing: bool,
}

impl UndoList {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            limit: 0,
            enabled: true,
            in_group: false,
            undoing: false,
        }
    }

    /// Enable or disable undo recording.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.records.clear();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set the maximum number of records (0 = unlimited).
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.maybe_truncate();
    }

    /// Record an insertion.
    pub fn record_insert(&mut self, pos: usize, len: usize) {
        if !self.enabled || self.undoing || len == 0 {
            return;
        }
        self.ensure_boundary_if_needed();
        self.records.push(UndoRecord::Insert { pos, len });
        self.in_group = true;
        self.maybe_truncate();
    }

    /// Record a deletion.
    pub fn record_delete(&mut self, pos: usize, text: &str) {
        if !self.enabled || self.undoing || text.is_empty() {
            return;
        }
        self.ensure_boundary_if_needed();
        self.records.push(UndoRecord::Delete {
            pos,
            text: text.to_string(),
        });
        self.in_group = true;
        self.maybe_truncate();
    }

    /// Record a cursor movement.
    pub fn record_cursor(&mut self, pos: usize) {
        if !self.enabled || self.undoing {
            return;
        }
        // Don't record consecutive cursor moves to the same position.
        if let Some(UndoRecord::CursorMove { pos: p }) = self.records.last() {
            if *p == pos {
                return;
            }
        }
        self.records.push(UndoRecord::CursorMove { pos });
    }

    /// Record a text property change.
    pub fn record_property_change(
        &mut self,
        pos: usize,
        len: usize,
        old_props: HashMap<String, Value>,
    ) {
        if !self.enabled || self.undoing {
            return;
        }
        self.ensure_boundary_if_needed();
        self.records.push(UndoRecord::PropertyChange {
            pos,
            len,
            old_props,
        });
        self.in_group = true;
    }

    /// Insert an undo boundary.
    pub fn boundary(&mut self) {
        if !self.enabled {
            return;
        }
        // Don't add consecutive boundaries.
        if matches!(self.records.last(), Some(UndoRecord::Boundary) | None) {
            return;
        }
        self.records.push(UndoRecord::Boundary);
        self.in_group = false;
    }

    /// Pop one undo group (everything until the next boundary or end).
    /// Returns the records in reverse order (most recent first) for
    /// the caller to apply.
    pub fn pop_undo_group(&mut self) -> Vec<UndoRecord> {
        let mut group = Vec::new();

        // Skip trailing boundary if present.
        while matches!(self.records.last(), Some(UndoRecord::Boundary)) {
            self.records.pop();
        }

        // Pop records until we hit a boundary or run out.
        while let Some(record) = self.records.last() {
            if matches!(record, UndoRecord::Boundary) {
                break;
            }
            group.push(self.records.pop().unwrap());
        }

        group
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// True when the most recent undo entry is a boundary marker.
    pub fn has_trailing_boundary(&self) -> bool {
        matches!(self.records.last(), Some(UndoRecord::Boundary))
    }

    /// True when the undo list contains at least one boundary marker.
    pub fn contains_boundary(&self) -> bool {
        self.records
            .iter()
            .any(|record| matches!(record, UndoRecord::Boundary))
    }

    /// Clear all undo history.
    pub fn clear(&mut self) {
        self.records.clear();
        self.in_group = false;
    }

    /// Convert to a Lisp value for `buffer-undo-list`.
    pub fn to_value(&self) -> Value {
        let mut items = Vec::new();
        for record in &self.records {
            match record {
                UndoRecord::Insert { pos, len } => {
                    // (BEG . END) — 1-based positions
                    items.push(Value::cons(
                        Value::Int((*pos + 1) as i64),
                        Value::Int((*pos + *len + 1) as i64),
                    ));
                }
                UndoRecord::Delete { pos, text } => {
                    // (TEXT . POS) — deleted text and 1-based position
                    items.push(Value::cons(
                        Value::string(text.clone()),
                        Value::Int((*pos + 1) as i64),
                    ));
                }
                UndoRecord::CursorMove { pos } => {
                    items.push(Value::Int((*pos + 1) as i64));
                }
                UndoRecord::PropertyChange { pos, len, .. } => {
                    // (nil PROPERTY VALUE BEG . END)
                    items.push(Value::cons(
                        Value::Nil,
                        Value::cons(
                            Value::Int((*pos + 1) as i64),
                            Value::Int((*pos + *len + 1) as i64),
                        ),
                    ));
                }
                UndoRecord::Boundary => {
                    items.push(Value::Nil);
                }
            }
        }
        Value::list(items)
    }

    // -- internal helpers ---

    fn ensure_boundary_if_needed(&mut self) {
        if !self.in_group && !matches!(self.records.last(), Some(UndoRecord::Boundary) | None) {
            self.records.push(UndoRecord::Boundary);
        }
    }

    fn maybe_truncate(&mut self) {
        if self.limit > 0 && self.records.len() > self.limit {
            let excess = self.records.len() - self.limit;
            self.records.drain(..excess);
        }
    }

    // pdump accessors
    pub(crate) fn dump_records(&self) -> &[UndoRecord] { &self.records }
    pub(crate) fn dump_limit(&self) -> usize { self.limit }
    pub(crate) fn dump_enabled(&self) -> bool { self.enabled }
    pub(crate) fn from_dump(records: Vec<UndoRecord>, limit: usize, enabled: bool) -> Self {
        Self { records, limit, enabled, in_group: false, undoing: false }
    }
}

impl Default for UndoList {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for UndoList {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for record in &self.records {
            if let UndoRecord::PropertyChange { old_props, .. } = record {
                for value in old_props.values() {
                    roots.push(*value);
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_insert_undo() {
        let mut undo = UndoList::new();
        undo.record_insert(0, 5);
        undo.record_insert(5, 3);
        undo.boundary();

        let group = undo.pop_undo_group();
        assert_eq!(group.len(), 2);
        // Most recent first
        assert!(matches!(group[0], UndoRecord::Insert { pos: 5, len: 3 }));
        assert!(matches!(group[1], UndoRecord::Insert { pos: 0, len: 5 }));
    }

    #[test]
    fn delete_records_text() {
        let mut undo = UndoList::new();
        undo.record_delete(3, "hello");
        undo.boundary();

        let group = undo.pop_undo_group();
        assert_eq!(group.len(), 1);
        match &group[0] {
            UndoRecord::Delete { pos, text } => {
                assert_eq!(*pos, 3);
                assert_eq!(text, "hello");
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn boundary_separates_groups() {
        let mut undo = UndoList::new();
        undo.record_insert(0, 1); // group 1
        undo.boundary();
        undo.record_insert(1, 1); // group 2
        undo.boundary();

        let g2 = undo.pop_undo_group();
        assert_eq!(g2.len(), 1);
        assert!(matches!(g2[0], UndoRecord::Insert { pos: 1, .. }));

        let g1 = undo.pop_undo_group();
        assert_eq!(g1.len(), 1);
        assert!(matches!(g1[0], UndoRecord::Insert { pos: 0, .. }));
    }

    #[test]
    fn disabled_records_nothing() {
        let mut undo = UndoList::new();
        undo.set_enabled(false);
        undo.record_insert(0, 5);
        assert!(undo.is_empty());
    }

    #[test]
    fn limit_truncates() {
        let mut undo = UndoList::new();
        undo.set_limit(3);
        for i in 0..10 {
            undo.record_insert(i, 1);
        }
        assert!(undo.len() <= 3);
    }

    #[test]
    fn cursor_move_dedup() {
        let mut undo = UndoList::new();
        undo.record_cursor(5);
        undo.record_cursor(5);
        undo.record_cursor(5);
        assert_eq!(undo.len(), 1);
        undo.record_cursor(10);
        assert_eq!(undo.len(), 2);
    }

    #[test]
    fn no_double_boundary() {
        let mut undo = UndoList::new();
        undo.record_insert(0, 1);
        undo.boundary();
        undo.boundary();
        undo.boundary();
        // Only one boundary after the insert
        assert_eq!(undo.len(), 2);
    }

    #[test]
    fn to_value_produces_list() {
        let mut undo = UndoList::new();
        undo.record_insert(0, 5);
        undo.boundary();
        let val = undo.to_value();
        assert!(val.is_list());
    }

    #[test]
    fn undoing_flag_suppresses() {
        let mut undo = UndoList::new();
        undo.undoing = true;
        undo.record_insert(0, 5);
        assert!(undo.is_empty());
        undo.undoing = false;
        undo.record_insert(0, 5);
        assert_eq!(undo.len(), 1);
    }
}
