//! Pure Rust text property system.
//!
//! Implements the core of Emacs's text property mechanism (cf. `textprop.c`
//! and `intervals.c`). Text properties are key-value pairs attached to
//! ranges of text in a buffer or string.
//!
//! # Property lists
//!
//! Properties are stored as alternating key-value pairs in a
//! [`PropertyList`], mirroring Emacs's plist representation. Each
//! property name is a string (corresponding to an Emacs symbol name).
//!
//! # Interval management
//!
//! [`TextProperties`] manages a sorted list of non-overlapping
//! [`TextPropertyInterval`]s. Adjacent intervals with identical
//! properties are automatically merged to minimize storage.
//!
//! # Stickiness
//!
//! By default, text properties are "rear-sticky": inserting text at
//! the end of a propertied region extends that region to cover the
//! new text. The [`Stickiness`] enum captures the three possible
//! behaviors.
//!
//! # Buffer adjustment
//!
//! When text is inserted or deleted, [`TextProperties::adjust_for_insert`]
//! and [`TextProperties::adjust_for_delete`] shift or truncate intervals
//! so they remain consistent with the buffer contents.

use std::fmt;

// ---------------------------------------------------------------------------
// PropertyValue
// ---------------------------------------------------------------------------

/// A value that can be stored as a text property.
///
/// Mirrors the Lisp value types that commonly appear in text property
/// plists: symbols, integers, floats, strings, booleans, nested lists,
/// and nil.
#[derive(Debug, Clone)]
pub enum PropertyValue {
    /// A symbol name (e.g., "face", "invisible").
    Symbol(String),
    /// An integer value.
    Integer(i64),
    /// A floating-point value.
    Float(f64),
    /// A string value.
    Str(String),
    /// A boolean value.
    Bool(bool),
    /// A nested list of property values.
    List(Vec<PropertyValue>),
    /// The nil/empty value.
    Nil,
}

impl PartialEq for PropertyValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PropertyValue::Symbol(a), PropertyValue::Symbol(b)) => a == b,
            (PropertyValue::Integer(a), PropertyValue::Integer(b)) => a == b,
            (PropertyValue::Float(a), PropertyValue::Float(b)) => a.to_bits() == b.to_bits(),
            (PropertyValue::Str(a), PropertyValue::Str(b)) => a == b,
            (PropertyValue::Bool(a), PropertyValue::Bool(b)) => a == b,
            (PropertyValue::List(a), PropertyValue::List(b)) => a == b,
            (PropertyValue::Nil, PropertyValue::Nil) => true,
            _ => false,
        }
    }
}

impl Eq for PropertyValue {}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PropertyValue::Symbol(s) => write!(f, "'{}", s),
            PropertyValue::Integer(n) => write!(f, "{}", n),
            PropertyValue::Float(v) => write!(f, "{}", v),
            PropertyValue::Str(s) => write!(f, "\"{}\"", s),
            PropertyValue::Bool(b) => write!(f, "{}", if *b { "t" } else { "nil" }),
            PropertyValue::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, ")")
            }
            PropertyValue::Nil => write!(f, "nil"),
        }
    }
}

// ---------------------------------------------------------------------------
// PropertyList
// ---------------------------------------------------------------------------

/// A property list (plist) storing alternating key-value pairs.
///
/// Keys are property names (strings corresponding to Emacs symbol names).
/// Each key appears at most once; duplicate keys are not permitted.
#[derive(Debug, Clone, Default)]
pub struct PropertyList {
    /// The property entries, stored as (key, value) pairs.
    entries: Vec<(String, PropertyValue)>,
}

impl PropertyList {
    /// Create a new empty property list.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Look up a property by name.
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Set a property value. If the key already exists, its value is
    /// replaced; otherwise a new entry is appended.
    pub fn put(&mut self, key: &str, value: PropertyValue) {
        for entry in &mut self.entries {
            if entry.0 == key {
                entry.1 = value;
                return;
            }
        }
        self.entries.push((key.to_string(), value));
    }

    /// Remove a property by name, returning the old value if present.
    pub fn remove(&mut self, key: &str) -> Option<PropertyValue> {
        if let Some(idx) = self.entries.iter().position(|(k, _)| k == key) {
            Some(self.entries.remove(idx).1)
        } else {
            None
        }
    }

    /// Return `true` if the property list contains the given key.
    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// Merge properties from `other` into this list. Existing keys are
    /// overwritten with values from `other`.
    pub fn merge(&mut self, other: &PropertyList) {
        for (key, value) in &other.entries {
            self.put(key, value.clone());
        }
    }

    /// Return `true` if the property list has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterate over (key, value) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PropertyValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
}

impl PartialEq for PropertyList {
    fn eq(&self, other: &Self) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        // Every key in self must exist in other with the same value.
        for (key, val) in &self.entries {
            match other.get(key) {
                Some(other_val) if other_val == val => {}
                _ => return false,
            }
        }
        true
    }
}

impl Eq for PropertyList {}

// ---------------------------------------------------------------------------
// Stickiness
// ---------------------------------------------------------------------------

/// Controls how text properties behave at insertion boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stickiness {
    /// Properties extend to cover text inserted at the front of the region.
    FrontSticky,
    /// Properties extend to cover text inserted at the rear of the region
    /// (this is the default Emacs behavior).
    RearSticky,
    /// Properties do not extend in either direction.
    Neither,
}

impl Default for Stickiness {
    fn default() -> Self {
        Stickiness::RearSticky
    }
}

// ---------------------------------------------------------------------------
// TextPropertyInterval
// ---------------------------------------------------------------------------

/// A half-open range `[start, end)` with an associated property list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPropertyInterval {
    /// Start of the interval (inclusive, 0-based).
    pub start: usize,
    /// End of the interval (exclusive, 0-based).
    pub end: usize,
    /// Properties attached to this range.
    pub properties: PropertyList,
}

impl TextPropertyInterval {
    /// Create a new interval.
    pub fn new(start: usize, end: usize, properties: PropertyList) -> Self {
        debug_assert!(start <= end);
        Self {
            start,
            end,
            properties,
        }
    }

    /// Length of the interval in characters.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Return `true` if the interval has zero length.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

// ---------------------------------------------------------------------------
// TextProperties
// ---------------------------------------------------------------------------

/// Manages text properties for a buffer or string.
///
/// Internally maintains a sorted list of non-overlapping intervals.
/// Adjacent intervals with identical property lists are merged
/// automatically after mutation operations.
pub struct TextProperties {
    /// Sorted, non-overlapping intervals.
    intervals: Vec<TextPropertyInterval>,
}

impl TextProperties {
    /// Create a new empty text property manager.
    pub fn new() -> Self {
        Self {
            intervals: Vec::new(),
        }
    }

    /// Return the number of intervals (useful for testing).
    pub fn interval_count(&self) -> usize {
        self.intervals.len()
    }

    // =================================================================
    // Queries
    // =================================================================

    /// Get a specific property value at `pos`.
    pub fn get_property(&self, pos: usize, key: &str) -> Option<&PropertyValue> {
        self.find_interval_at(pos)
            .and_then(|idx| self.intervals[idx].properties.get(key))
    }

    /// Get all properties at `pos`, returning a cloned property list.
    /// If no interval covers `pos`, an empty list is returned.
    pub fn get_all_properties(&self, pos: usize) -> PropertyList {
        match self.find_interval_at(pos) {
            Some(idx) => self.intervals[idx].properties.clone(),
            None => PropertyList::new(),
        }
    }

    /// Find the next position (strictly greater than `pos`) where any
    /// property changes. Returns `None` if there is no change after `pos`.
    pub fn next_property_change(&self, pos: usize) -> Option<usize> {
        // If pos is before all intervals, the first interval start is the
        // next change.
        if self.intervals.is_empty() {
            return None;
        }

        match self.find_interval_at(pos) {
            Some(idx) => {
                // pos is inside intervals[idx]. The next change is at
                // intervals[idx].end, unless that end butts up against
                // a successor with identical properties.
                let mut boundary = self.intervals[idx].end;
                let mut next_idx = idx + 1;
                while next_idx < self.intervals.len()
                    && self.intervals[next_idx].start == boundary
                    && self.intervals[next_idx].properties == self.intervals[idx].properties
                {
                    boundary = self.intervals[next_idx].end;
                    next_idx += 1;
                }
                if boundary > pos { Some(boundary) } else { None }
            }
            None => {
                // pos is not inside any interval. Find the next interval
                // that starts after pos.
                for iv in &self.intervals {
                    if iv.start > pos {
                        return Some(iv.start);
                    }
                }
                None
            }
        }
    }

    /// Find the previous position (strictly less than `pos`) where any
    /// property changes. Returns `None` if there is no change before `pos`.
    pub fn previous_property_change(&self, pos: usize) -> Option<usize> {
        if self.intervals.is_empty() || pos == 0 {
            return None;
        }

        // Collect all property-change boundaries strictly less than pos.
        // A boundary occurs at the start and end of every interval,
        // except where adjacent intervals have identical properties
        // (those are effectively one region).
        let mut best: Option<usize> = None;

        for iv in &self.intervals {
            // Start of interval is a boundary (transition from no-props
            // or different-props to this interval's props).
            if iv.start > 0 && iv.start < pos {
                match best {
                    Some(b) if b >= iv.start => {}
                    _ => best = Some(iv.start),
                }
            }
            // End of interval is a boundary (transition away from this
            // interval's props).
            if iv.end < pos {
                match best {
                    Some(b) if b >= iv.end => {}
                    _ => best = Some(iv.end),
                }
            }
        }

        best
    }

    /// Find the next position where the specific property `key` changes
    /// value (compared by equality).
    pub fn next_single_property_change(&self, pos: usize, key: &str) -> Option<usize> {
        let current_val = self.get_property(pos, key);

        // Walk forward through intervals.
        for iv in &self.intervals {
            if iv.end <= pos {
                continue;
            }
            if iv.start > pos || (iv.start <= pos && iv.end > pos) {
                let iv_val = iv.properties.get(key);
                // Check if the value is different from current.
                let same = match (current_val, iv_val) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if !same && iv.start > pos {
                    return Some(iv.start);
                }
                // If same, skip past this interval and keep looking.
                if !same && iv.start <= pos {
                    // Shouldn't happen since current_val came from this pos.
                    return Some(iv.end);
                }
            }
        }

        // Check if current_val is Some and we run past all intervals
        // (transition to no-property zone).
        if current_val.is_some() {
            if let Some(idx) = self.find_interval_at(pos) {
                // Walk forward to find where key changes.
                let mut i = idx;
                while i < self.intervals.len() {
                    let iv_val = self.intervals[i].properties.get(key);
                    let same = match (current_val, iv_val) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    };
                    if !same {
                        return Some(self.intervals[i].start);
                    }
                    // Check for gap between intervals.
                    if i + 1 < self.intervals.len()
                        && self.intervals[i].end < self.intervals[i + 1].start
                    {
                        return Some(self.intervals[i].end);
                    }
                    i += 1;
                }
                // Ran off the end; property goes to "none" after last interval.
                if i > 0 {
                    return Some(self.intervals[i - 1].end);
                }
            }
        } else {
            // current_val is None. Find the first interval after pos that
            // has this key.
            for iv in &self.intervals {
                if iv.end <= pos {
                    continue;
                }
                if iv.start > pos && iv.properties.contains(key) {
                    return Some(iv.start);
                }
            }
        }

        None
    }

    /// Return the extent `(start, end)` of the region around `pos` where
    /// the specific property `key` has the same value.
    pub fn property_extent(&self, pos: usize, key: &str) -> Option<(usize, usize)> {
        let idx = self.find_interval_at(pos)?;
        let val = self.intervals[idx].properties.get(key)?;

        let mut start = self.intervals[idx].start;
        let mut end = self.intervals[idx].end;

        // Extend backwards.
        let mut i = idx;
        while i > 0 {
            let prev = &self.intervals[i - 1];
            if prev.end == start {
                if let Some(prev_val) = prev.properties.get(key) {
                    if prev_val == val {
                        start = prev.start;
                        i -= 1;
                        continue;
                    }
                }
            }
            break;
        }

        // Extend forwards.
        let mut j = idx + 1;
        while j < self.intervals.len() {
            let next = &self.intervals[j];
            if next.start == end {
                if let Some(next_val) = next.properties.get(key) {
                    if next_val == val {
                        end = next.end;
                        j += 1;
                        continue;
                    }
                }
            }
            break;
        }

        Some((start, end))
    }

    /// Determine the stickiness of a property at `pos`.
    ///
    /// In standard Emacs behavior:
    /// - `front-sticky` properties on the character *at* pos make the
    ///   property front-sticky.
    /// - Otherwise properties are rear-sticky by default.
    /// - If the property `front-sticky` or `rear-nonsticky` lists include
    ///   the key, stickiness is adjusted accordingly.
    ///
    /// This simplified implementation checks for `front-sticky` and
    /// `rear-nonsticky` meta-properties on the interval.
    pub fn text_property_stickiness(&self, pos: usize, key: &str) -> Stickiness {
        // Check the interval at pos for front-sticky.
        if let Some(idx) = self.find_interval_at(pos) {
            let iv = &self.intervals[idx];
            if is_property_in_list(&iv.properties, "front-sticky", key) {
                return Stickiness::FrontSticky;
            }
            if is_property_in_list(&iv.properties, "rear-nonsticky", key) {
                return Stickiness::Neither;
            }
        }
        // Check the previous interval for rear-nonsticky.
        if pos > 0 {
            if let Some(idx) = self.find_interval_at(pos.saturating_sub(1)) {
                let iv = &self.intervals[idx];
                if iv.end == pos && is_property_in_list(&iv.properties, "rear-nonsticky", key) {
                    return Stickiness::Neither;
                }
            }
        }
        Stickiness::RearSticky
    }

    // =================================================================
    // Mutation
    // =================================================================

    /// Set a single property on the range `[start, end)`.
    ///
    /// This is the equivalent of Emacs `put-text-property`. The property
    /// is merged into existing intervals, splitting them as necessary.
    /// Adjacent intervals with identical properties are merged.
    pub fn put_property(&mut self, start: usize, end: usize, key: &str, value: PropertyValue) {
        if start >= end {
            return;
        }

        // Collect indices of intervals that overlap [start, end).
        let overlapping = self.find_overlapping(start, end);

        if overlapping.is_empty() {
            // No existing intervals overlap this range. Just insert.
            let mut props = PropertyList::new();
            props.put(key, value);
            self.insert_interval(TextPropertyInterval::new(start, end, props));
            self.merge_adjacent();
            return;
        }

        // We need to handle the general case: split intervals at the
        // boundaries, then set the property on all intervals within range.
        self.split_at(start);
        self.split_at(end);

        // Fill any gaps in the range [start, end) with empty-property
        // intervals so we have full coverage.
        self.fill_gaps(start, end);

        // Now set the property on all intervals within [start, end).
        for iv in &mut self.intervals {
            if iv.start >= end {
                break;
            }
            if iv.end <= start {
                continue;
            }
            // This interval overlaps [start, end).
            iv.properties.put(key, value.clone());
        }

        self.merge_adjacent();
    }

    /// Remove a specific property from the range `[start, end)`.
    pub fn remove_property(&mut self, start: usize, end: usize, key: &str) {
        if start >= end {
            return;
        }

        self.split_at(start);
        self.split_at(end);

        let mut to_remove = Vec::new();

        for (i, iv) in self.intervals.iter_mut().enumerate() {
            if iv.start >= end {
                break;
            }
            if iv.end <= start {
                continue;
            }
            iv.properties.remove(key);
            if iv.properties.is_empty() {
                to_remove.push(i);
            }
        }

        // Remove empty intervals in reverse order to preserve indices.
        for &idx in to_remove.iter().rev() {
            self.intervals.remove(idx);
        }

        self.merge_adjacent();
    }

    /// Remove all properties from the range `[start, end)`.
    pub fn remove_all_properties(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        self.split_at(start);
        self.split_at(end);

        self.intervals
            .retain(|iv| iv.end <= start || iv.start >= end);

        self.merge_adjacent();
    }

    // =================================================================
    // Buffer adjustment
    // =================================================================

    /// Adjust intervals after text insertion at `pos` of `length` characters.
    ///
    /// Intervals are rear-sticky by default:
    /// - Intervals that end at `pos` are extended to cover the new text.
    /// - Intervals that start after `pos` are shifted right.
    /// - Intervals that straddle `pos` are expanded.
    pub fn adjust_for_insert(&mut self, pos: usize, length: usize) {
        if length == 0 {
            return;
        }

        for iv in &mut self.intervals {
            if iv.end <= pos {
                // Interval is entirely before insertion: check rear-stickiness.
                // By default (rear-sticky), intervals ending exactly at pos
                // extend to cover inserted text.
                if iv.end == pos {
                    iv.end += length;
                }
                continue;
            }

            if iv.start > pos {
                // Interval is entirely after insertion: shift right.
                iv.start += length;
                iv.end += length;
            } else {
                // Interval straddles pos: expand.
                iv.end += length;
            }
        }

        self.merge_adjacent();
    }

    /// Adjust intervals after text deletion from `from` to `to` (exclusive).
    ///
    /// - Intervals entirely within `[from, to)` are removed.
    /// - Intervals that straddle the deleted range are truncated.
    /// - Intervals after the deletion are shifted left.
    pub fn adjust_for_delete(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }

        let length = to - from;
        let mut to_remove = Vec::new();

        for (i, iv) in self.intervals.iter_mut().enumerate() {
            if iv.end <= from {
                // Entirely before deletion: no change.
                continue;
            }

            if iv.start >= to {
                // Entirely after deletion: shift left.
                iv.start -= length;
                iv.end -= length;
                continue;
            }

            // Interval overlaps the deleted region.
            if iv.start >= from && iv.end <= to {
                // Entirely within deleted region: remove.
                to_remove.push(i);
            } else if iv.start < from && iv.end > to {
                // Interval straddles the entire deletion: shrink.
                iv.end -= length;
            } else if iv.start < from {
                // Interval starts before and overlaps into deletion: truncate end.
                iv.end = from;
            } else {
                // Interval starts within deletion and extends past: truncate start.
                iv.start = from;
                iv.end -= length;
            }
        }

        for &idx in to_remove.iter().rev() {
            self.intervals.remove(idx);
        }

        // Remove zero-length intervals.
        self.intervals.retain(|iv| iv.start < iv.end);

        self.merge_adjacent();
    }

    // =================================================================
    // Internal helpers
    // =================================================================

    /// Find the index of the interval containing `pos`, if any.
    fn find_interval_at(&self, pos: usize) -> Option<usize> {
        // Binary search for the interval containing pos.
        if self.intervals.is_empty() {
            return None;
        }

        let mut lo = 0;
        let mut hi = self.intervals.len();

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.intervals[mid].end <= pos {
                lo = mid + 1;
            } else if self.intervals[mid].start > pos {
                hi = mid;
            } else {
                return Some(mid);
            }
        }

        None
    }

    /// Find indices of all intervals overlapping `[start, end)`.
    fn find_overlapping(&self, start: usize, end: usize) -> Vec<usize> {
        let mut result = Vec::new();
        for (i, iv) in self.intervals.iter().enumerate() {
            if iv.start >= end {
                break;
            }
            if iv.end > start {
                result.push(i);
            }
        }
        result
    }

    /// Split any interval at boundary `pos`, so that no interval straddles
    /// `pos`. This is a no-op if `pos` is already on a boundary or outside
    /// all intervals.
    fn split_at(&mut self, pos: usize) {
        if let Some(idx) = self.find_interval_at(pos) {
            let iv = &self.intervals[idx];
            if iv.start == pos || iv.end == pos {
                // Already on a boundary.
                return;
            }
            // Split: [iv.start, pos) and [pos, iv.end)
            let second = TextPropertyInterval::new(pos, iv.end, iv.properties.clone());
            self.intervals[idx].end = pos;
            self.intervals.insert(idx + 1, second);
        }
    }

    /// Fill gaps in the interval list within `[start, end)` with
    /// empty-property intervals.
    fn fill_gaps(&mut self, start: usize, end: usize) {
        let mut gaps = Vec::new();
        let mut cursor = start;

        for iv in &self.intervals {
            if iv.start >= end {
                break;
            }
            if iv.end <= start {
                continue;
            }
            if iv.start > cursor {
                gaps.push((cursor, iv.start));
            }
            cursor = iv.end;
        }

        if cursor < end {
            gaps.push((cursor, end));
        }

        for (gs, ge) in gaps {
            self.insert_interval(TextPropertyInterval::new(gs, ge, PropertyList::new()));
        }
    }

    /// Insert an interval into the sorted list, maintaining order by start.
    fn insert_interval(&mut self, interval: TextPropertyInterval) {
        let pos = self
            .intervals
            .iter()
            .position(|iv| iv.start >= interval.start)
            .unwrap_or(self.intervals.len());
        self.intervals.insert(pos, interval);
    }

    /// Merge adjacent intervals that have identical property lists.
    fn merge_adjacent(&mut self) {
        if self.intervals.len() < 2 {
            return;
        }

        let mut i = 0;
        while i + 1 < self.intervals.len() {
            if self.intervals[i].end == self.intervals[i + 1].start
                && self.intervals[i].properties == self.intervals[i + 1].properties
            {
                self.intervals[i].end = self.intervals[i + 1].end;
                self.intervals.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

impl Default for TextProperties {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether `meta_key` is a property on `plist` whose value is a list
/// containing `target_key` as a symbol, or is `Bool(true)` / `Symbol("t")`
/// (meaning "all properties").
fn is_property_in_list(plist: &PropertyList, meta_key: &str, target_key: &str) -> bool {
    match plist.get(meta_key) {
        None | Some(PropertyValue::Nil) => false,
        Some(PropertyValue::Bool(true)) => true,
        Some(PropertyValue::Symbol(s)) if s == "t" => true,
        Some(PropertyValue::List(items)) => items.iter().any(|item| match item {
            PropertyValue::Symbol(s) => s == target_key,
            PropertyValue::Str(s) => s == target_key,
            _ => false,
        }),
        Some(PropertyValue::Symbol(s)) => s == target_key,
        _ => false,
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- PropertyValue tests --

    #[test]
    fn test_property_value_equality() {
        assert_eq!(PropertyValue::Nil, PropertyValue::Nil);
        assert_eq!(PropertyValue::Integer(42), PropertyValue::Integer(42));
        assert_ne!(PropertyValue::Integer(1), PropertyValue::Integer(2));
        assert_eq!(
            PropertyValue::Symbol("face".into()),
            PropertyValue::Symbol("face".into())
        );
        assert_ne!(
            PropertyValue::Symbol("face".into()),
            PropertyValue::Symbol("invisible".into())
        );
        assert_eq!(PropertyValue::Float(1.5), PropertyValue::Float(1.5));
        assert_ne!(PropertyValue::Nil, PropertyValue::Bool(false));
        assert_eq!(
            PropertyValue::List(vec![PropertyValue::Integer(1)]),
            PropertyValue::List(vec![PropertyValue::Integer(1)])
        );
    }

    #[test]
    fn test_property_value_display() {
        assert_eq!(format!("{}", PropertyValue::Nil), "nil");
        assert_eq!(format!("{}", PropertyValue::Integer(42)), "42");
        assert_eq!(format!("{}", PropertyValue::Symbol("face".into())), "'face");
        assert_eq!(
            format!("{}", PropertyValue::Str("hello".into())),
            "\"hello\""
        );
        assert_eq!(format!("{}", PropertyValue::Bool(true)), "t");
        assert_eq!(format!("{}", PropertyValue::Bool(false)), "nil");
    }

    // -- PropertyList tests --

    #[test]
    fn test_property_list_basic() {
        let mut plist = PropertyList::new();
        assert!(plist.is_empty());
        assert_eq!(plist.len(), 0);

        plist.put("face", PropertyValue::Symbol("bold".into()));
        assert!(!plist.is_empty());
        assert_eq!(plist.len(), 1);
        assert!(plist.contains("face"));
        assert!(!plist.contains("invisible"));
        assert_eq!(
            plist.get("face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
    }

    #[test]
    fn test_property_list_put_overwrite() {
        let mut plist = PropertyList::new();
        plist.put("face", PropertyValue::Symbol("bold".into()));
        plist.put("face", PropertyValue::Symbol("italic".into()));
        assert_eq!(plist.len(), 1);
        assert_eq!(
            plist.get("face"),
            Some(&PropertyValue::Symbol("italic".into()))
        );
    }

    #[test]
    fn test_property_list_remove() {
        let mut plist = PropertyList::new();
        plist.put("face", PropertyValue::Symbol("bold".into()));
        plist.put("invisible", PropertyValue::Bool(true));

        let removed = plist.remove("face");
        assert_eq!(removed, Some(PropertyValue::Symbol("bold".into())));
        assert_eq!(plist.len(), 1);
        assert!(!plist.contains("face"));

        let removed2 = plist.remove("nonexistent");
        assert_eq!(removed2, None);
    }

    #[test]
    fn test_property_list_merge() {
        let mut a = PropertyList::new();
        a.put("face", PropertyValue::Symbol("bold".into()));
        a.put("invisible", PropertyValue::Bool(true));

        let mut b = PropertyList::new();
        b.put("face", PropertyValue::Symbol("italic".into()));
        b.put("read-only", PropertyValue::Bool(true));

        a.merge(&b);
        assert_eq!(a.len(), 3);
        assert_eq!(a.get("face"), Some(&PropertyValue::Symbol("italic".into())));
        assert_eq!(a.get("invisible"), Some(&PropertyValue::Bool(true)));
        assert_eq!(a.get("read-only"), Some(&PropertyValue::Bool(true)));
    }

    #[test]
    fn test_property_list_equality() {
        let mut a = PropertyList::new();
        a.put("face", PropertyValue::Symbol("bold".into()));
        a.put("invisible", PropertyValue::Bool(true));

        let mut b = PropertyList::new();
        b.put("invisible", PropertyValue::Bool(true));
        b.put("face", PropertyValue::Symbol("bold".into()));

        // Order-independent equality.
        assert_eq!(a, b);

        b.put("extra", PropertyValue::Nil);
        assert_ne!(a, b);
    }

    #[test]
    fn test_property_list_iter() {
        let mut plist = PropertyList::new();
        plist.put("a", PropertyValue::Integer(1));
        plist.put("b", PropertyValue::Integer(2));

        let items: Vec<_> = plist.iter().collect();
        assert_eq!(items.len(), 2);
        assert!(items.contains(&("a", &PropertyValue::Integer(1))));
        assert!(items.contains(&("b", &PropertyValue::Integer(2))));
    }

    // -- TextPropertyInterval tests --

    #[test]
    fn test_interval_basic() {
        let iv = TextPropertyInterval::new(5, 10, PropertyList::new());
        assert_eq!(iv.start, 5);
        assert_eq!(iv.end, 10);
        assert_eq!(iv.len(), 5);
        assert!(!iv.is_empty());
    }

    #[test]
    fn test_interval_empty() {
        let iv = TextPropertyInterval::new(5, 5, PropertyList::new());
        assert!(iv.is_empty());
        assert_eq!(iv.len(), 0);
    }

    // -- TextProperties: put/get tests --

    #[test]
    fn test_put_and_get_property() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 10, "face", PropertyValue::Symbol("bold".into()));

        assert_eq!(
            tp.get_property(5, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(9, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        // Position 10 is exclusive.
        assert_eq!(tp.get_property(10, "face"), None);
        // Position 4 is before the range.
        assert_eq!(tp.get_property(4, "face"), None);
    }

    #[test]
    fn test_put_property_overlapping() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 15, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(10, 20, "face", PropertyValue::Symbol("italic".into()));

        // [5, 10) should still be bold
        assert_eq!(
            tp.get_property(7, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        // [10, 20) should be italic
        assert_eq!(
            tp.get_property(12, "face"),
            Some(&PropertyValue::Symbol("italic".into()))
        );
    }

    #[test]
    fn test_put_property_multiple_keys() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(0, 10, "invisible", PropertyValue::Bool(true));

        let plist = tp.get_all_properties(5);
        assert_eq!(plist.len(), 2);
        assert_eq!(
            plist.get("face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(plist.get("invisible"), Some(&PropertyValue::Bool(true)));
    }

    #[test]
    fn test_put_property_merges_adjacent() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 5, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(5, 10, "face", PropertyValue::Symbol("bold".into()));

        // Should merge into a single interval.
        assert_eq!(tp.interval_count(), 1);
        assert_eq!(
            tp.get_property(0, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(9, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
    }

    #[test]
    fn test_put_property_does_not_merge_different() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 5, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(5, 10, "face", PropertyValue::Symbol("italic".into()));

        assert_eq!(tp.interval_count(), 2);
    }

    // -- TextProperties: remove tests --

    #[test]
    fn test_remove_property() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(0, 10, "invisible", PropertyValue::Bool(true));

        tp.remove_property(0, 10, "face");

        assert_eq!(tp.get_property(5, "face"), None);
        assert_eq!(
            tp.get_property(5, "invisible"),
            Some(&PropertyValue::Bool(true))
        );
    }

    #[test]
    fn test_remove_property_partial() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 20, "face", PropertyValue::Symbol("bold".into()));

        tp.remove_property(5, 15, "face");

        // [0, 5) still has face.
        assert_eq!(
            tp.get_property(2, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        // [5, 15) no longer has face.
        assert_eq!(tp.get_property(10, "face"), None);
        // [15, 20) still has face.
        assert_eq!(
            tp.get_property(17, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
    }

    #[test]
    fn test_remove_all_properties() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 20, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(0, 20, "invisible", PropertyValue::Bool(true));

        tp.remove_all_properties(5, 15);

        assert_eq!(tp.get_property(2, "face").is_some(), true);
        assert_eq!(tp.get_property(10, "face"), None);
        assert_eq!(tp.get_property(10, "invisible"), None);
        assert_eq!(tp.get_property(17, "face").is_some(), true);
    }

    // -- TextProperties: next/previous property change --

    #[test]
    fn test_next_property_change() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(15, 20, "face", PropertyValue::Symbol("italic".into()));

        // From position 0, next change is at 5 (start of first interval).
        assert_eq!(tp.next_property_change(0), Some(5));
        // From inside the first interval, next change is at its end.
        assert_eq!(tp.next_property_change(7), Some(10));
        // From the gap, next change is at 15.
        assert_eq!(tp.next_property_change(12), Some(15));
        // Past all intervals: no more changes.
        assert_eq!(tp.next_property_change(20), None);
    }

    #[test]
    fn test_previous_property_change() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(15, 20, "face", PropertyValue::Symbol("italic".into()));

        // From 20, previous change is at start of last interval.
        assert_eq!(tp.previous_property_change(20), Some(15));
        // From inside second interval, previous change is at its start.
        assert_eq!(tp.previous_property_change(17), Some(15));
        // From gap between intervals, previous change is at end of first.
        assert_eq!(tp.previous_property_change(12), Some(10));
        // From inside first interval, previous change is at its start.
        assert_eq!(tp.previous_property_change(7), Some(5));
        // Before all intervals.
        assert_eq!(tp.previous_property_change(3), None);
    }

    #[test]
    fn test_next_single_property_change() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(10, 20, "face", PropertyValue::Symbol("italic".into()));

        // "face" changes at position 10.
        assert_eq!(tp.next_single_property_change(5, "face"), Some(10));
        // "invisible" doesn't exist anywhere.
        assert_eq!(tp.next_single_property_change(5, "invisible"), None);
    }

    // -- TextProperties: property_extent --

    #[test]
    fn test_property_extent() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(10, 20, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(20, 30, "face", PropertyValue::Symbol("italic".into()));

        // Adjacent bold intervals should merge, so extent is [0, 20).
        assert_eq!(tp.property_extent(5, "face"), Some((0, 20)));
        assert_eq!(tp.property_extent(15, "face"), Some((0, 20)));
        assert_eq!(tp.property_extent(25, "face"), Some((20, 30)));
    }

    // -- TextProperties: buffer adjustment --

    #[test]
    fn test_adjust_for_insert_at_end_rear_sticky() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        // Insert 5 chars at position 10 (rear-sticky extends).
        tp.adjust_for_insert(10, 5);

        assert_eq!(
            tp.get_property(12, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(14, "face").is_some(), true);
    }

    #[test]
    fn test_adjust_for_insert_in_middle() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        // Insert 5 chars at position 5 (within interval).
        tp.adjust_for_insert(5, 5);

        // Interval should expand: [0, 15)
        assert_eq!(
            tp.get_property(0, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(14, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(15, "face"), None);
    }

    #[test]
    fn test_adjust_for_insert_shifts_later() {
        let mut tp = TextProperties::new();
        tp.put_property(10, 20, "face", PropertyValue::Symbol("bold".into()));

        // Insert 5 chars at position 5 (before the interval).
        tp.adjust_for_insert(5, 5);

        // Interval should shift: [15, 25)
        assert_eq!(tp.get_property(10, "face"), None);
        assert_eq!(
            tp.get_property(15, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(24, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
    }

    #[test]
    fn test_adjust_for_delete_within() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 20, "face", PropertyValue::Symbol("bold".into()));

        // Delete [5, 10).
        tp.adjust_for_delete(5, 10);

        // Interval should shrink: [0, 15)
        assert_eq!(
            tp.get_property(0, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(14, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(15, "face"), None);
    }

    #[test]
    fn test_adjust_for_delete_entire_interval() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 10, "face", PropertyValue::Symbol("bold".into()));

        // Delete [3, 12) which encompasses the entire interval.
        tp.adjust_for_delete(3, 12);

        assert_eq!(tp.interval_count(), 0);
        assert_eq!(tp.get_property(5, "face"), None);
    }

    #[test]
    fn test_adjust_for_delete_shifts_later() {
        let mut tp = TextProperties::new();
        tp.put_property(10, 20, "face", PropertyValue::Symbol("bold".into()));

        // Delete [0, 5).
        tp.adjust_for_delete(0, 5);

        // Interval should shift: [5, 15)
        assert_eq!(
            tp.get_property(5, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(4, "face"), None);
    }

    // -- TextProperties: stickiness --

    #[test]
    fn test_stickiness_default_rear() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        assert_eq!(
            tp.text_property_stickiness(5, "face"),
            Stickiness::RearSticky
        );
    }

    #[test]
    fn test_stickiness_front_sticky() {
        let mut tp = TextProperties::new();
        tp.put_property(
            0,
            10,
            "front-sticky",
            PropertyValue::List(vec![PropertyValue::Symbol("face".into())]),
        );
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        assert_eq!(
            tp.text_property_stickiness(0, "face"),
            Stickiness::FrontSticky
        );
    }

    #[test]
    fn test_stickiness_rear_nonsticky() {
        let mut tp = TextProperties::new();
        tp.put_property(
            0,
            10,
            "rear-nonsticky",
            PropertyValue::List(vec![PropertyValue::Symbol("face".into())]),
        );
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        assert_eq!(tp.text_property_stickiness(5, "face"), Stickiness::Neither);
    }

    // -- Edge cases --

    #[test]
    fn test_empty_range_put() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 5, "face", PropertyValue::Symbol("bold".into()));

        // Empty range should be a no-op.
        assert_eq!(tp.interval_count(), 0);
    }

    #[test]
    fn test_get_all_properties_empty() {
        let tp = TextProperties::new();
        let plist = tp.get_all_properties(5);
        assert!(plist.is_empty());
    }

    #[test]
    fn test_next_property_change_empty() {
        let tp = TextProperties::new();
        assert_eq!(tp.next_property_change(0), None);
    }

    #[test]
    fn test_previous_property_change_empty() {
        let tp = TextProperties::new();
        assert_eq!(tp.previous_property_change(0), None);
    }

    #[test]
    fn test_property_extent_no_property() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 10, "face", PropertyValue::Symbol("bold".into()));

        assert_eq!(tp.property_extent(5, "invisible"), None);
    }

    #[test]
    fn test_put_property_gap_between_intervals() {
        let mut tp = TextProperties::new();
        tp.put_property(0, 5, "face", PropertyValue::Symbol("bold".into()));
        tp.put_property(10, 15, "face", PropertyValue::Symbol("bold".into()));

        // Apply a different property to the gap region.
        tp.put_property(5, 10, "invisible", PropertyValue::Bool(true));

        // The gap should now have the invisible property.
        assert_eq!(
            tp.get_property(7, "invisible"),
            Some(&PropertyValue::Bool(true))
        );
        // But not the face property.
        assert_eq!(tp.get_property(7, "face"), None);
    }

    #[test]
    fn test_large_range_operations() {
        let mut tp = TextProperties::new();

        // Set up many intervals.
        for i in 0..50 {
            tp.put_property(i * 10, i * 10 + 5, "face", PropertyValue::Integer(i as i64));
        }

        // Remove property from a large range spanning many intervals.
        tp.remove_property(50, 400, "face");

        // First 5 intervals should still have their values.
        for i in 0..5 {
            assert_eq!(
                tp.get_property(i * 10 + 2, "face"),
                Some(&PropertyValue::Integer(i as i64))
            );
        }
        // Middle should be gone.
        assert_eq!(tp.get_property(100, "face"), None);
        // Last intervals should still be present.
        assert_eq!(
            tp.get_property(402, "face"),
            Some(&PropertyValue::Integer(40))
        );
    }

    #[test]
    fn test_adjust_for_delete_partial_overlap_start() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 15, "face", PropertyValue::Symbol("bold".into()));

        // Delete [0, 8): overlaps the start of the interval.
        tp.adjust_for_delete(0, 8);

        // Interval should become [0, 7).
        assert_eq!(
            tp.get_property(0, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(6, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(7, "face"), None);
    }

    #[test]
    fn test_adjust_for_delete_partial_overlap_end() {
        let mut tp = TextProperties::new();
        tp.put_property(5, 15, "face", PropertyValue::Symbol("bold".into()));

        // Delete [10, 20): overlaps the end of the interval.
        tp.adjust_for_delete(10, 20);

        // Interval should be truncated: [5, 10).
        assert_eq!(
            tp.get_property(5, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(
            tp.get_property(9, "face"),
            Some(&PropertyValue::Symbol("bold".into()))
        );
        assert_eq!(tp.get_property(10, "face"), None);
    }
}
