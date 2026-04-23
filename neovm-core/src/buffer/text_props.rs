//! Text properties system for buffers.
//!
//! Text properties are key-value pairs attached to ranges of text within a
//! buffer. They are indexed by interval start boundaries, with each interval
//! carrying a set of properties. When a property is set on a range, existing
//! intervals are split at the boundaries and the property is applied to all
//! affected intervals. Adjacent intervals with identical property sets are
//! merged to keep the interval map compact.

use std::collections::{BTreeMap, HashMap};

use crate::emacs_core::value::{Value, equal_value};
use crate::gc_trace::GcTrace;

// ---------------------------------------------------------------------------
// PropertyInterval
// ---------------------------------------------------------------------------

/// A single text property interval: [start, end) with properties.
///
/// Each interval covers a half-open byte range and holds a map of Lisp-valued
/// properties. GNU Emacs stores interval properties in Lisp plists and
/// compares property identity by Lisp object identity, not by Rust string
/// contents. We mirror that here by keeping Lisp `Value` keys and preserving
/// plist order separately.
#[derive(Clone, Debug)]
pub struct PropertyInterval {
    /// Byte position where this interval starts (inclusive).
    pub start: usize,
    /// Byte position where this interval ends (exclusive).
    pub end: usize,
    /// The property map for this interval.
    pub properties: HashMap<Value, Value>,
    /// Property keys in insertion order (most recently added first,
    /// matching GNU Emacs's prepend semantics).
    pub(crate) key_order: Vec<Value>,
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

    pub fn with_properties(start: usize, end: usize, properties: HashMap<Value, Value>) -> Self {
        let key_order: Vec<Value> = properties.keys().copied().collect();
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
    fn insert_property(&mut self, name: Value, value: Value) -> bool {
        let already_equal = self
            .properties
            .get(&name)
            .map_or(false, |existing| equal_value(existing, &value, 0));
        if already_equal {
            return false;
        }
        let is_new = !self.properties.contains_key(&name);
        self.properties.insert(name, value);
        if is_new {
            // Prepend new properties (GNU Emacs behavior)
            self.key_order.insert(0, name);
        }
        true
    }

    /// Remove a property by name.
    fn remove_property(&mut self, name: Value) -> Option<Value> {
        let result = self.properties.remove(&name);
        if result.is_some() {
            self.key_order.retain(|k| *k != name);
        }
        result
    }

    /// Returns true if the interval has no properties.
    fn is_empty_props(&self) -> bool {
        self.properties.is_empty()
    }

    /// Iterate properties in insertion order (most recently added first).
    pub fn ordered_properties(&self) -> impl Iterator<Item = (Value, &Value)> {
        self.key_order
            .iter()
            .filter_map(move |k| self.properties.get(k).map(|v| (*k, v)))
    }
}

// ---------------------------------------------------------------------------
// Helper: compare two property maps for structural equality
// ---------------------------------------------------------------------------

fn props_equal(a: &HashMap<Value, Value>, b: &HashMap<Value, Value>) -> bool {
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
/// Internally stores a start-boundary-indexed, non-overlapping set of
/// [`PropertyInterval`]s. Intervals with empty property sets may exist
/// transiently but are cleaned up during merge passes.
#[derive(Clone)]
pub struct TextPropertyTable {
    intervals: BTreeMap<usize, PropertyInterval>,
}

impl TextPropertyTable {
    /// Create an empty property table.
    pub fn new() -> Self {
        Self {
            intervals: BTreeMap::new(),
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
    pub fn put_property(&mut self, start: usize, end: usize, name: Value, value: Value) -> bool {
        if start >= end {
            return false;
        }

        self.split_at(start);
        self.split_at(end);

        // Ensure there is coverage for the entire [start, end) range.
        self.ensure_coverage(start, end);

        let mut changed = false;
        let keys: Vec<usize> = self
            .intervals
            .range(start..end)
            .map(|(&key, _)| key)
            .collect();
        for key in keys {
            if let Some(interval) = self.intervals.get_mut(&key)
                && interval.insert_property(name, value)
            {
                changed = true;
            }
        }

        self.merge_adjacent_around(start, end);
        changed
    }

    /// Get a single property at a byte position.
    pub fn get_property(&self, pos: usize, name: Value) -> Option<&Value> {
        self.interval_containing(pos)
            .and_then(|interval| interval.properties.get(&name))
    }

    /// Get all properties at a byte position.
    pub fn get_properties(&self, pos: usize) -> HashMap<Value, Value> {
        self.interval_containing(pos)
            .map(|interval| interval.properties.clone())
            .unwrap_or_default()
    }

    /// Get all properties at a byte position in insertion order (most recently added first).
    /// Returns a list of (name, value) pairs in the order matching GNU Emacs plist output.
    pub fn get_properties_ordered(&self, pos: usize) -> Vec<(Value, Value)> {
        self.interval_containing(pos)
            .map(|interval| {
                interval
                    .ordered_properties()
                    .map(|(k, v)| (k, *v))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Remove a single named property from the byte range `[start, end)`.
    /// Returns `true` if any property was actually removed, `false` otherwise.
    pub fn remove_property(&mut self, start: usize, end: usize, name: Value) -> bool {
        if start >= end {
            return false;
        }

        self.split_at(start);
        self.split_at(end);

        let mut removed = false;
        let keys: Vec<usize> = self
            .intervals
            .range(start..end)
            .map(|(&key, _)| key)
            .collect();
        for key in keys {
            if let Some(interval) = self.intervals.get_mut(&key)
                && interval.remove_property(name).is_some()
            {
                removed = true;
            }
        }

        // Remove empty intervals and merge only the affected neighborhood.
        self.cleanup_range(start, end);
        self.merge_adjacent_around(start, end);
        removed
    }

    /// Remove all properties from the byte range `[start, end)`.
    pub fn remove_all_properties(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        self.split_at(start);
        self.split_at(end);

        let keys: Vec<usize> = self
            .intervals
            .range(start..end)
            .map(|(&key, _)| key)
            .collect();
        for key in keys {
            if let Some(interval) = self.intervals.get_mut(&key) {
                interval.properties.clear();
                interval.key_order.clear();
            }
        }

        self.cleanup_range(start, end);
        self.merge_adjacent_around(start, end);
    }

    /// Return the next position at or after `pos` where any text property
    /// changes, or `None` if there is no change after `pos`.
    pub fn next_property_change(&self, pos: usize) -> Option<usize> {
        if let Some(interval) = self.interval_containing(pos) {
            return Some(interval.end);
        }
        self.intervals
            .range((std::ops::Bound::Excluded(pos), std::ops::Bound::Unbounded))
            .next()
            .map(|(_, interval)| interval.start)
    }

    /// Return the previous position before `pos` where any text property
    /// changes, or `None` if there is no change before `pos`.
    pub fn previous_property_change(&self, pos: usize) -> Option<usize> {
        if let Some(interval) = self.interval_containing(pos.saturating_sub(1))
            && pos <= interval.end
            && interval.start < pos
        {
            return Some(interval.start);
        }
        self.intervals
            .range(..pos)
            .next_back()
            .map(|(_, interval)| interval.end)
    }

    /// Adjust all intervals after text is inserted at `pos` with `len` bytes.
    ///
    /// Matches GNU Emacs `adjust_intervals_for_insertion` (intervals.c:802):
    ///
    /// - Intervals starting AFTER the insertion point are shifted right.
    /// - An interval whose interior CONTAINS the insertion point is SPLIT
    ///   around the inserted range, leaving the newly inserted text without
    ///   inherited properties.
    /// - An interval whose START equals `pos` is shifted right (the inserted
    ///   text goes BEFORE this interval, not inside it).
    /// - `insert-and-inherit` and related commands compute stickiness-aware
    ///   inherited properties separately after the structural split.
    pub fn adjust_for_insert(&mut self, pos: usize, len: usize) {
        if len == 0 {
            return;
        }
        let mut shifted = BTreeMap::new();
        for interval in self.intervals_snapshot() {
            if interval.start == pos {
                let mut shifted_interval = interval;
                shifted_interval.start += len;
                shifted_interval.end += len;
                shifted.insert(shifted_interval.start, shifted_interval);
            } else if interval.start > pos {
                let mut shifted_interval = interval;
                shifted_interval.start += len;
                shifted_interval.end += len;
                shifted.insert(shifted_interval.start, shifted_interval);
            } else if interval.end > pos {
                let mut left = interval.clone();
                left.end = pos;
                if left.start < left.end {
                    shifted.insert(left.start, left);
                }

                let mut right = interval;
                right.start = pos + len;
                right.end += len;
                if right.start < right.end {
                    shifted.insert(right.start, right);
                }
            } else {
                shifted.insert(interval.start, interval);
            }
        }
        self.intervals = shifted;
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
        let mut shifted = BTreeMap::new();

        for mut interval in self.intervals_snapshot() {
            if interval.start >= end {
                interval.start -= len;
                interval.end -= len;
            } else if interval.end <= start {
            } else if interval.start >= start && interval.end <= end {
                continue;
            } else if interval.start < start && interval.end > end {
                interval.end -= len;
            } else if interval.start < start {
                interval.end = start;
            } else {
                interval.start = start;
                interval.end -= len;
            }
            shifted.insert(interval.start, interval);
        }
        self.intervals = shifted;
        self.merge_adjacent();
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Split any interval that spans `pos` into two intervals at `pos`.
    fn split_at(&mut self, pos: usize) {
        let Some((&start, interval)) = self.intervals.range(..pos).next_back() else {
            return;
        };
        if !(interval.start < pos && pos < interval.end) {
            return;
        }

        let second = PropertyInterval {
            start: pos,
            end: interval.end,
            properties: interval.properties.clone(),
            key_order: interval.key_order.clone(),
        };
        if let Some(first) = self.intervals.get_mut(&start) {
            first.end = pos;
        }
        self.intervals.insert(pos, second);
    }

    /// Ensure that the entire range `[start, end)` is covered by intervals.
    /// Fill any gaps with empty-property intervals.
    fn ensure_coverage(&mut self, start: usize, end: usize) {
        let mut gaps = Vec::new();
        let mut cursor = start;

        for interval in self
            .intervals
            .range(start..end)
            .map(|(_, interval)| interval)
        {
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

        for (gap_start, gap_end) in gaps {
            self.intervals
                .insert(gap_start, PropertyInterval::new(gap_start, gap_end));
        }
    }

    /// Remove intervals with no properties.
    fn cleanup(&mut self) {
        self.intervals.retain(|_, iv| !iv.is_empty_props());
    }

    /// Remove empty intervals that can only have been created in `[start, end)`.
    fn cleanup_range(&mut self, start: usize, end: usize) {
        let keys: Vec<usize> = self
            .intervals
            .range(start..end)
            .filter_map(|(&key, interval)| interval.is_empty_props().then_some(key))
            .collect();
        for key in keys {
            self.intervals.remove(&key);
        }
    }

    /// Merge adjacent intervals that have identical property maps.
    fn merge_adjacent(&mut self) {
        if self.intervals.len() < 2 {
            return;
        }

        let mut merged = BTreeMap::new();
        let mut current: Option<PropertyInterval> = None;

        for interval in self.intervals.values().cloned() {
            match current.take() {
                None => current = Some(interval),
                Some(mut active) => {
                    if active.end == interval.start
                        && props_equal(&active.properties, &interval.properties)
                    {
                        active.end = interval.end;
                        current = Some(active);
                    } else {
                        merged.insert(active.start, active);
                        current = Some(interval);
                    }
                }
            }
        }
        if let Some(interval) = current {
            merged.insert(interval.start, interval);
        }
        self.intervals = merged;
    }

    /// Merge adjacent equal intervals only near a changed range.
    ///
    /// GNU's interval operations split and update the affected interval chain;
    /// they don't rescan the whole buffer after every property write.  The
    /// only possible new merge points are inside the changed range and at its
    /// two boundaries, so restrict compaction to that neighborhood.
    fn merge_adjacent_around(&mut self, start: usize, end: usize) {
        if self.intervals.len() < 2 {
            return;
        }

        let mut keys = Vec::new();
        if let Some((&key, _)) = self.intervals.range(..=start).next_back() {
            keys.push(key);
        }
        keys.extend(self.intervals.range(start..=end).map(|(&key, _)| key));
        if let Some((&key, _)) = self
            .intervals
            .range((std::ops::Bound::Excluded(end), std::ops::Bound::Unbounded))
            .next()
        {
            keys.push(key);
        }

        keys.sort_unstable();
        keys.dedup();
        if keys.len() < 2 {
            return;
        }

        let mut intervals: Vec<PropertyInterval> = keys
            .into_iter()
            .filter_map(|key| self.intervals.remove(&key))
            .collect();
        intervals.sort_by_key(|interval| interval.start);

        let mut merged: Vec<PropertyInterval> = Vec::with_capacity(intervals.len());
        for interval in intervals {
            if let Some(active) = merged.last_mut()
                && active.end == interval.start
                && props_equal(&active.properties, &interval.properties)
            {
                active.end = interval.end;
                continue;
            }
            merged.push(interval);
        }

        for interval in merged {
            self.intervals.insert(interval.start, interval);
        }
    }

    fn interval_containing(&self, pos: usize) -> Option<&PropertyInterval> {
        let (_, interval) = self.intervals.range(..=pos).next_back()?;
        (interval.start <= pos && pos < interval.end).then_some(interval)
    }

    /// Expose a stable interval snapshot for iteration (GC tracing, printing, etc.).
    pub fn intervals_snapshot(&self) -> Vec<PropertyInterval> {
        self.intervals.values().cloned().collect()
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
        let mut result = BTreeMap::new();
        for iv in self.intervals.values() {
            if iv.end <= start || iv.start >= end {
                continue;
            }
            let new_start = iv.start.max(start) - start;
            let new_end = iv.end.min(end) - start;
            if new_start < new_end && !iv.properties.is_empty() {
                result.insert(
                    new_start,
                    PropertyInterval {
                        start: new_start,
                        end: new_end,
                        properties: iv.properties.clone(),
                        key_order: iv.key_order.clone(),
                    },
                );
            }
        }
        TextPropertyTable { intervals: result }
    }

    /// Append another table's intervals shifted by `byte_offset`.
    pub fn append_shifted(&mut self, other: &TextPropertyTable, byte_offset: usize) {
        for iv in other.intervals.values() {
            if iv.properties.is_empty() {
                continue;
            }
            let start = iv.start + byte_offset;
            self.intervals.insert(
                start,
                PropertyInterval {
                    start,
                    end: iv.end + byte_offset,
                    properties: iv.properties.clone(),
                    key_order: iv.key_order.clone(),
                },
            );
        }
        self.merge_adjacent();
    }

    // pdump accessors
    pub(crate) fn dump_intervals(&self) -> Vec<PropertyInterval> {
        self.intervals_snapshot()
    }
    pub(crate) fn from_dump(intervals: Vec<PropertyInterval>) -> Self {
        Self {
            intervals: intervals
                .into_iter()
                .map(|interval| (interval.start, interval))
                .collect(),
        }
    }

    pub(crate) fn for_each_root(&self, mut f: impl FnMut(Value)) {
        for interval in self.intervals.values() {
            for key in interval.properties.keys() {
                f(*key);
            }
            for value in interval.properties.values() {
                f(*value);
            }
        }
    }
}

impl Default for TextPropertyTable {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for TextPropertyTable {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        self.for_each_root(|value| roots.push(value));
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "text_props_test.rs"]
mod tests;
