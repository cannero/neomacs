//! Text properties system for buffers.
//!
//! Text properties are key-value pairs attached to ranges of text within a
//! buffer. They are stored as a sorted list of non-overlapping intervals,
//! each carrying a set of properties. When a property is set on a range,
//! existing intervals are split at the boundaries and the property is applied
//! to all affected intervals. Adjacent intervals with identical property sets
//! are merged to keep the list compact.

use std::collections::HashMap;

use crate::emacs_core::value::{Value, equal_value};
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// PropertyInterval
// ---------------------------------------------------------------------------

/// A single text property interval: [start, end) with properties.
///
/// Each interval covers a half-open byte range and holds a map of named
/// properties.  Properties are stored internally in a HashMap for efficient
/// lookup, but also maintain an insertion-order list so that serialization
/// to plists is deterministic (matching GNU Emacs's prepend-based ordering).
#[derive(Clone, Debug)]
pub struct PropertyInterval {
    /// Byte position where this interval starts (inclusive).
    pub start: usize,
    /// Byte position where this interval ends (exclusive).
    pub end: usize,
    /// The property map for this interval.
    pub properties: HashMap<String, Value>,
    /// Property names in insertion order (most recently added first,
    /// matching GNU Emacs's prepend semantics).
    pub(crate) key_order: Vec<String>,
}

impl PropertyInterval {
    fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            properties: HashMap::new(),
            key_order: Vec::new(),
        }
    }

    pub fn with_properties(start: usize, end: usize, properties: HashMap<String, Value>) -> Self {
        let key_order: Vec<String> = properties.keys().cloned().collect();
        Self {
            start,
            end,
            properties,
            key_order,
        }
    }

    /// Insert or update a property, maintaining key_order.
    /// New properties are prepended (matching GNU Emacs behavior).
    /// Returns true if the property was actually changed.
    fn insert_property(&mut self, name: &str, value: Value) -> bool {
        let already_equal = self
            .properties
            .get(name)
            .map_or(false, |existing| equal_value(existing, &value, 0));
        if already_equal {
            return false;
        }
        let is_new = !self.properties.contains_key(name);
        self.properties.insert(name.to_string(), value);
        if is_new {
            // Prepend new properties (GNU Emacs behavior)
            self.key_order.insert(0, name.to_string());
        }
        true
    }

    /// Remove a property by name.
    fn remove_property(&mut self, name: &str) -> Option<Value> {
        let result = self.properties.remove(name);
        if result.is_some() {
            self.key_order.retain(|k| k != name);
        }
        result
    }

    /// Returns true if the interval has no properties.
    fn is_empty_props(&self) -> bool {
        self.properties.is_empty()
    }

    /// Iterate properties in insertion order (most recently added first).
    pub fn ordered_properties(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.key_order.iter().filter_map(move |k| {
            self.properties.get(k).map(|v| (k.as_str(), v))
        })
    }
}

// ---------------------------------------------------------------------------
// Helper: compare two property maps for structural equality
// ---------------------------------------------------------------------------

fn props_equal(a: &HashMap<String, Value>, b: &HashMap<String, Value>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (key, val_a) in a {
        match b.get(key) {
            Some(val_b) => {
                if !equal_value(val_a, val_b, 0) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// TextPropertyTable
// ---------------------------------------------------------------------------

/// Manages text properties for a buffer.
///
/// Internally stores a sorted (by start position), non-overlapping list of
/// [`PropertyInterval`]s.  Intervals with empty property sets may exist
/// transiently but are cleaned up during merge passes.
#[derive(Clone)]
pub struct TextPropertyTable {
    intervals: Vec<PropertyInterval>,
}

impl TextPropertyTable {
    /// Create an empty property table.
    pub fn new() -> Self {
        Self {
            intervals: Vec::new(),
        }
    }

    /// Set a property on the byte range `[start, end)`.
    ///
    /// Any existing intervals that overlap the range are split at the
    /// boundaries, and the named property is set on all intervals within
    /// the range. Adjacent intervals with identical properties are then
    /// merged.
    ///
    /// Returns `true` if any property value was actually changed (or added),
    /// `false` if all intervals already had the property with an equal value.
    pub fn put_property(&mut self, start: usize, end: usize, name: &str, value: Value) -> bool {
        if start >= end {
            return false;
        }

        self.split_at(start);
        self.split_at(end);

        // Ensure there is coverage for the entire [start, end) range.
        self.ensure_coverage(start, end);

        let mut changed = false;
        // Set the property on all intervals within [start, end).
        for interval in &mut self.intervals {
            if interval.start >= end {
                break;
            }
            if interval.end <= start {
                continue;
            }
            // This interval overlaps [start, end).
            if interval.insert_property(name, value) {
                changed = true;
            }
        }

        self.merge_adjacent();
        changed
    }

    /// Get a single property at a byte position.
    pub fn get_property(&self, pos: usize, name: &str) -> Option<&Value> {
        for interval in &self.intervals {
            if interval.start > pos {
                break;
            }
            if pos >= interval.start && pos < interval.end {
                return interval.properties.get(name);
            }
        }
        None
    }

    /// Get all properties at a byte position.
    pub fn get_properties(&self, pos: usize) -> HashMap<String, Value> {
        for interval in &self.intervals {
            if interval.start > pos {
                break;
            }
            if pos >= interval.start && pos < interval.end {
                return interval.properties.clone();
            }
        }
        HashMap::new()
    }

    /// Get all properties at a byte position in insertion order (most recently added first).
    /// Returns a list of (name, value) pairs in the order matching GNU Emacs plist output.
    pub fn get_properties_ordered(&self, pos: usize) -> Vec<(String, Value)> {
        for interval in &self.intervals {
            if interval.start > pos {
                break;
            }
            if pos >= interval.start && pos < interval.end {
                return interval.ordered_properties()
                    .map(|(k, v)| (k.to_string(), *v))
                    .collect();
            }
        }
        Vec::new()
    }

    /// Remove a single named property from the byte range `[start, end)`.
    /// Returns `true` if any property was actually removed, `false` otherwise.
    pub fn remove_property(&mut self, start: usize, end: usize, name: &str) -> bool {
        if start >= end {
            return false;
        }

        self.split_at(start);
        self.split_at(end);

        let mut removed = false;
        for interval in &mut self.intervals {
            if interval.start >= end {
                break;
            }
            if interval.end <= start {
                continue;
            }
            if interval.remove_property(name).is_some() {
                removed = true;
            }
        }

        // Remove empty intervals and merge adjacent.
        self.cleanup();
        self.merge_adjacent();
        removed
    }

    /// Remove all properties from the byte range `[start, end)`.
    pub fn remove_all_properties(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        self.split_at(start);
        self.split_at(end);

        // Clear properties on all intervals within [start, end).
        for interval in &mut self.intervals {
            if interval.start >= end {
                break;
            }
            if interval.end <= start {
                continue;
            }
            interval.properties.clear();
            interval.key_order.clear();
        }

        self.cleanup();
        self.merge_adjacent();
    }

    /// Return the next position at or after `pos` where any text property
    /// changes, or `None` if there is no change after `pos`.
    pub fn next_property_change(&self, pos: usize) -> Option<usize> {
        for interval in &self.intervals {
            if interval.start > pos {
                return Some(interval.start);
            }
            if pos >= interval.start && pos < interval.end {
                return Some(interval.end);
            }
        }
        None
    }

    /// Return the previous position before `pos` where any text property
    /// changes, or `None` if there is no change before `pos`.
    pub fn previous_property_change(&self, pos: usize) -> Option<usize> {
        // Walk intervals in reverse to find the closest boundary before pos.
        for interval in self.intervals.iter().rev() {
            if interval.end <= pos {
                return Some(interval.end);
            }
            if interval.start < pos && pos <= interval.end {
                return Some(interval.start);
            }
        }
        None
    }

    /// Adjust all intervals after text is inserted at `pos` with `len` bytes.
    ///
    /// Intervals starting at or after the insertion point are shifted right.
    /// An interval spanning the insertion point is expanded.
    pub fn adjust_for_insert(&mut self, pos: usize, len: usize) {
        if len == 0 {
            return;
        }
        for interval in &mut self.intervals {
            if interval.start >= pos {
                // Interval starts at or after insertion: shift entirely.
                interval.start += len;
                interval.end += len;
            } else if interval.end > pos {
                // Interval spans the insertion point: expand it.
                interval.end += len;
            }
            // Interval entirely before insertion: unchanged.
        }
    }

    /// Adjust all intervals after text in `[start, end)` is deleted.
    ///
    /// Intervals inside the deleted range are removed or truncated.
    /// Intervals after the deleted range are shifted left.
    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;

        let mut to_remove = Vec::new();

        for (i, interval) in self.intervals.iter_mut().enumerate() {
            if interval.start >= end {
                // Entirely after deletion: shift left.
                interval.start -= len;
                interval.end -= len;
            } else if interval.end <= start {
                // Entirely before deletion: unchanged.
            } else if interval.start >= start && interval.end <= end {
                // Entirely within deleted range: mark for removal.
                to_remove.push(i);
            } else if interval.start < start && interval.end > end {
                // Spans the entire deleted range: shrink.
                interval.end -= len;
            } else if interval.start < start {
                // Overlaps start of deletion: truncate end.
                interval.end = start;
            } else {
                // interval.start >= start && interval.end > end
                // Overlaps end of deletion: adjust start and shift.
                interval.start = start;
                interval.end -= len;
            }
        }

        // Remove intervals in reverse order to keep indices valid.
        for &i in to_remove.iter().rev() {
            self.intervals.remove(i);
        }

        self.merge_adjacent();
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Split any interval that spans `pos` into two intervals at `pos`.
    fn split_at(&mut self, pos: usize) {
        for i in 0..self.intervals.len() {
            let interval = &self.intervals[i];
            if interval.start < pos && pos < interval.end {
                // Split this interval, preserving key_order.
                let second = PropertyInterval {
                    start: pos,
                    end: interval.end,
                    properties: interval.properties.clone(),
                    key_order: interval.key_order.clone(),
                };
                self.intervals[i].end = pos;
                self.intervals.insert(i + 1, second);
                return;
            }
        }
    }

    /// Ensure that the entire range `[start, end)` is covered by intervals.
    /// Fill any gaps with empty-property intervals.
    fn ensure_coverage(&mut self, start: usize, end: usize) {
        // Collect gaps.
        let mut gaps = Vec::new();
        let mut cursor = start;

        for interval in &self.intervals {
            if interval.start >= end {
                break;
            }
            if interval.end <= cursor {
                continue;
            }
            if interval.start > cursor {
                gaps.push((cursor, interval.start));
            }
            if interval.end > cursor {
                cursor = interval.end;
            }
        }
        if cursor < end {
            gaps.push((cursor, end));
        }

        // Insert gap intervals.
        for (gap_start, gap_end) in gaps {
            let new_interval = PropertyInterval::new(gap_start, gap_end);
            // Find insertion position to maintain sorted order.
            let pos = self
                .intervals
                .iter()
                .position(|iv| iv.start >= gap_start)
                .unwrap_or(self.intervals.len());
            self.intervals.insert(pos, new_interval);
        }
    }

    /// Remove intervals with no properties.
    fn cleanup(&mut self) {
        self.intervals.retain(|iv| !iv.is_empty_props());
    }

    /// Merge adjacent intervals that have identical property maps.
    fn merge_adjacent(&mut self) {
        if self.intervals.len() < 2 {
            return;
        }

        let mut i = 0;
        while i + 1 < self.intervals.len() {
            let can_merge = self.intervals[i].end == self.intervals[i + 1].start
                && props_equal(
                    &self.intervals[i].properties,
                    &self.intervals[i + 1].properties,
                );

            if can_merge {
                self.intervals[i].end = self.intervals[i + 1].end;
                self.intervals.remove(i + 1);
                // Don't advance i — check if the newly merged interval
                // can also merge with the next one.
            } else {
                i += 1;
            }
        }
    }

    /// Expose intervals for iteration (GC tracing, printing, etc.).
    pub fn intervals(&self) -> &[PropertyInterval] {
        &self.intervals
    }

    /// Returns true if there are no intervals (no properties).
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// Extract a sub-range `[start, end)` of the property table,
    /// shifting all positions to be 0-based relative to `start`.
    pub fn slice(&self, start: usize, end: usize) -> TextPropertyTable {
        if start >= end {
            return TextPropertyTable::new();
        }
        let mut result = Vec::new();
        for iv in &self.intervals {
            if iv.end <= start || iv.start >= end {
                continue;
            }
            let new_start = iv.start.max(start) - start;
            let new_end = iv.end.min(end) - start;
            if new_start < new_end && !iv.properties.is_empty() {
                result.push(PropertyInterval {
                    start: new_start,
                    end: new_end,
                    properties: iv.properties.clone(),
                    key_order: iv.key_order.clone(),
                });
            }
        }
        TextPropertyTable { intervals: result }
    }

    /// Append another table's intervals shifted by `byte_offset`.
    pub fn append_shifted(&mut self, other: &TextPropertyTable, byte_offset: usize) {
        for iv in &other.intervals {
            if iv.properties.is_empty() {
                continue;
            }
            self.intervals.push(PropertyInterval {
                start: iv.start + byte_offset,
                end: iv.end + byte_offset,
                properties: iv.properties.clone(),
                key_order: iv.key_order.clone(),
            });
        }
        self.merge_adjacent();
    }

    // pdump accessors
    pub(crate) fn dump_intervals(&self) -> &[PropertyInterval] {
        &self.intervals
    }
    pub(crate) fn from_dump(intervals: Vec<PropertyInterval>) -> Self {
        Self { intervals }
    }
}

impl Default for TextPropertyTable {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for TextPropertyTable {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for interval in &self.intervals {
            for value in interval.properties.values() {
                roots.push(*value);
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

    // -----------------------------------------------------------------------
    // Basic put/get
    // -----------------------------------------------------------------------

    #[test]
    fn put_and_get_basic() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 5, "face", Value::symbol("bold"));

        assert!(table.get_property(0, "face").is_some());
        assert!(table.get_property(2, "face").is_some());
        assert!(table.get_property(4, "face").is_some());
        assert!(table.get_property(5, "face").is_none()); // exclusive end
    }

    #[test]
    fn get_property_returns_correct_value() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        let val = table.get_property(5, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "bold")
        );
    }

    #[test]
    fn get_property_nonexistent_name() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        assert!(table.get_property(5, "syntax-table").is_none());
    }

    #[test]
    fn get_properties_returns_all() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(0, 10, "help-echo", Value::string("tooltip"));
        let props = table.get_properties(5);
        assert_eq!(props.len(), 2);
        assert!(props.contains_key("face"));
        assert!(props.contains_key("help-echo"));
    }

    #[test]
    fn get_property_outside_any_interval() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));
        assert!(table.get_property(0, "face").is_none());
        assert!(table.get_property(3, "face").is_none());
        assert!(table.get_property(10, "face").is_none());
        assert!(table.get_property(15, "face").is_none());
    }

    // -----------------------------------------------------------------------
    // Overlapping ranges
    // -----------------------------------------------------------------------

    #[test]
    fn overlapping_put_splits_intervals() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(5, 15, "face", Value::symbol("italic"));

        // [0, 5) should still have "bold"
        let val = table.get_property(3, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "bold")
        );

        // [5, 15) should have "italic" (overwritten)
        let val = table.get_property(7, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "italic")
        );

        let val = table.get_property(12, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "italic")
        );
    }

    #[test]
    fn multiple_properties_on_same_range() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(0, 10, "mouse-face", Value::symbol("highlight"));

        let props = table.get_properties(5);
        assert_eq!(props.len(), 2);
    }

    #[test]
    fn put_property_inner_range() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 20, "face", Value::symbol("default"));
        table.put_property(5, 15, "face", Value::symbol("bold"));

        let val = table.get_property(3, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "default")
        );

        let val = table.get_property(10, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "bold")
        );

        let val = table.get_property(17, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "default")
        );
    }

    #[test]
    fn put_different_properties_on_overlapping_ranges() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(5, 15, "syntax-table", Value::Int(42));

        // Position 3: only "face"
        let props = table.get_properties(3);
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("face"));

        // Position 7: both "face" and "syntax-table"
        let props = table.get_properties(7);
        assert_eq!(props.len(), 2);

        // Position 12: only "syntax-table"
        let props = table.get_properties(12);
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("syntax-table"));
    }

    // -----------------------------------------------------------------------
    // Remove
    // -----------------------------------------------------------------------

    #[test]
    fn remove_property_basic() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(0, 10, "help-echo", Value::string("help"));

        table.remove_property(0, 10, "face");

        assert!(table.get_property(5, "face").is_none());
        assert!(table.get_property(5, "help-echo").is_some());
    }

    #[test]
    fn remove_property_partial_range() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));

        table.remove_property(3, 7, "face");

        // [0, 3) still has face
        assert!(table.get_property(2, "face").is_some());
        // [3, 7) no longer has face
        assert!(table.get_property(5, "face").is_none());
        // [7, 10) still has face
        assert!(table.get_property(8, "face").is_some());
    }

    #[test]
    fn remove_all_properties_basic() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(0, 10, "help-echo", Value::string("help"));

        table.remove_all_properties(0, 10);

        assert!(table.get_property(5, "face").is_none());
        assert!(table.get_property(5, "help-echo").is_none());
    }

    #[test]
    fn remove_all_properties_partial() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));

        table.remove_all_properties(3, 7);

        assert!(table.get_property(2, "face").is_some());
        assert!(table.get_property(5, "face").is_none());
        assert!(table.get_property(8, "face").is_some());
    }

    // -----------------------------------------------------------------------
    // next/previous property change
    // -----------------------------------------------------------------------

    #[test]
    fn next_property_change_basic() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));
        table.put_property(15, 20, "face", Value::symbol("italic"));

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
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        // At start of interval
        assert_eq!(table.next_property_change(5), Some(10));
    }

    #[test]
    fn previous_property_change_basic() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));
        table.put_property(15, 20, "face", Value::symbol("italic"));

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
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        // At exclusive end of interval
        assert_eq!(table.previous_property_change(10), Some(10));
    }

    #[test]
    fn next_previous_empty_table() {
        let table = TextPropertyTable::new();
        assert_eq!(table.next_property_change(0), None);
        assert_eq!(table.previous_property_change(10), None);
    }

    // -----------------------------------------------------------------------
    // adjust_for_insert
    // -----------------------------------------------------------------------

    #[test]
    fn adjust_insert_shifts_intervals_after() {
        let mut table = TextPropertyTable::new();
        table.put_property(10, 20, "face", Value::symbol("bold"));

        table.adjust_for_insert(5, 3);

        // Interval should now be [13, 23)
        assert!(table.get_property(12, "face").is_none());
        assert!(table.get_property(13, "face").is_some());
        assert!(table.get_property(22, "face").is_some());
        assert!(table.get_property(23, "face").is_none());
    }

    #[test]
    fn adjust_insert_expands_spanning_interval() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 15, "face", Value::symbol("bold"));

        table.adjust_for_insert(10, 5);

        // Interval should now be [5, 20)
        assert!(table.get_property(5, "face").is_some());
        assert!(table.get_property(12, "face").is_some());
        assert!(table.get_property(19, "face").is_some());
        assert!(table.get_property(20, "face").is_none());
    }

    #[test]
    fn adjust_insert_at_interval_start() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        table.adjust_for_insert(5, 3);

        // Interval should shift to [8, 13)
        assert!(table.get_property(7, "face").is_none());
        assert!(table.get_property(8, "face").is_some());
        assert!(table.get_property(12, "face").is_some());
        assert!(table.get_property(13, "face").is_none());
    }

    #[test]
    fn adjust_insert_before_all() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        table.adjust_for_insert(0, 2);

        assert!(table.get_property(7, "face").is_some());
        assert!(table.get_property(6, "face").is_none());
    }

    #[test]
    fn adjust_insert_zero_length() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        table.adjust_for_insert(7, 0);

        // No change
        assert!(table.get_property(5, "face").is_some());
        assert!(table.get_property(9, "face").is_some());
        assert!(table.get_property(10, "face").is_none());
    }

    // -----------------------------------------------------------------------
    // adjust_for_delete
    // -----------------------------------------------------------------------

    #[test]
    fn adjust_delete_shifts_intervals_after() {
        let mut table = TextPropertyTable::new();
        table.put_property(10, 20, "face", Value::symbol("bold"));

        table.adjust_for_delete(2, 5);

        // 3 bytes deleted before interval; interval becomes [7, 17)
        assert!(table.get_property(6, "face").is_none());
        assert!(table.get_property(7, "face").is_some());
        assert!(table.get_property(16, "face").is_some());
        assert!(table.get_property(17, "face").is_none());
    }

    #[test]
    fn adjust_delete_removes_contained_interval() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        table.adjust_for_delete(3, 12);

        // Entire interval was within deleted range
        assert!(table.get_property(5, "face").is_none());
        assert!(table.get_property(3, "face").is_none());
    }

    #[test]
    fn adjust_delete_truncates_start() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 15, "face", Value::symbol("bold"));

        table.adjust_for_delete(10, 20);

        // Deletion overlaps end of interval; truncated to [5, 10)
        assert!(table.get_property(5, "face").is_some());
        assert!(table.get_property(9, "face").is_some());
        assert!(table.get_property(10, "face").is_none());
    }

    #[test]
    fn adjust_delete_shrinks_spanning_interval() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 20, "face", Value::symbol("bold"));

        table.adjust_for_delete(10, 15);

        // Deletion within interval; shrinks to [5, 15)
        assert!(table.get_property(5, "face").is_some());
        assert!(table.get_property(14, "face").is_some());
        assert!(table.get_property(15, "face").is_none());
    }

    #[test]
    fn adjust_delete_overlaps_interval_start() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 15, "face", Value::symbol("bold"));

        table.adjust_for_delete(2, 10);

        // Deletion overlaps beginning of interval: [5,15) minus [2,10)
        // After: interval becomes [2, 7) (shifted: start=2, end=15-8=7)
        assert!(table.get_property(2, "face").is_some());
        assert!(table.get_property(6, "face").is_some());
        assert!(table.get_property(7, "face").is_none());
    }

    #[test]
    fn adjust_delete_empty_range() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 10, "face", Value::symbol("bold"));

        table.adjust_for_delete(7, 7);

        // No change
        assert!(table.get_property(5, "face").is_some());
        assert!(table.get_property(9, "face").is_some());
    }

    // -----------------------------------------------------------------------
    // Merge adjacent intervals
    // -----------------------------------------------------------------------

    #[test]
    fn merge_adjacent_same_properties() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 5, "face", Value::symbol("bold"));
        table.put_property(5, 10, "face", Value::symbol("bold"));

        // After put, adjacent intervals with same properties should merge.
        // We can verify by checking that only one interval exists.
        assert!(table.get_property(0, "face").is_some());
        assert!(table.get_property(7, "face").is_some());

        // next_property_change from 0 should go to 10 (not 5)
        assert_eq!(table.next_property_change(0), Some(10));
    }

    #[test]
    fn no_merge_different_properties() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 5, "face", Value::symbol("bold"));
        table.put_property(5, 10, "face", Value::symbol("italic"));

        // Should remain as two intervals.
        assert_eq!(table.next_property_change(0), Some(5));
        assert_eq!(table.next_property_change(5), Some(10));
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn put_property_empty_range() {
        let mut table = TextPropertyTable::new();
        table.put_property(5, 5, "face", Value::symbol("bold"));
        assert!(table.get_property(5, "face").is_none());
    }

    #[test]
    fn put_property_overwrites_same_name() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 10, "face", Value::symbol("bold"));
        table.put_property(0, 10, "face", Value::symbol("italic"));

        let val = table.get_property(5, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "italic")
        );
    }

    #[test]
    fn multiple_non_contiguous_intervals() {
        let mut table = TextPropertyTable::new();
        table.put_property(0, 5, "face", Value::symbol("bold"));
        table.put_property(10, 15, "face", Value::symbol("italic"));
        table.put_property(20, 25, "face", Value::symbol("underline"));

        assert!(table.get_property(3, "face").is_some());
        assert!(table.get_property(7, "face").is_none());
        assert!(table.get_property(12, "face").is_some());
        assert!(table.get_property(17, "face").is_none());
        assert!(table.get_property(22, "face").is_some());
    }
}
