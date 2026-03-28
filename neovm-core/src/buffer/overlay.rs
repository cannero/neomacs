//! Overlay system for buffers.
//!
//! Overlays are like text properties but are attached to the buffer rather
//! than the text itself. Each overlay has a start and end position, a set
//! of properties, and flags controlling whether its endpoints advance when
//! text is inserted at the boundary.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound::{Excluded, Unbounded};

use crate::emacs_core::value::Value;
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

/// A single overlay covering a byte range `[start, end)` with properties.
#[derive(Clone, Debug)]
pub struct Overlay {
    /// Unique identifier for this overlay.
    pub id: u64,
    /// Start byte position (inclusive).
    pub start: usize,
    /// End byte position (exclusive).
    pub end: usize,
    /// Named properties on this overlay.
    pub properties: HashMap<String, Value>,
    /// If true, the start position advances when text is inserted at exactly
    /// the start position. (Like `InsertionType::After` for markers.)
    pub front_advance: bool,
    /// If true, the end position advances when text is inserted at exactly
    /// the end position. (Like `InsertionType::After` for markers.)
    pub rear_advance: bool,
}

// ---------------------------------------------------------------------------
// OverlayList
// ---------------------------------------------------------------------------

/// Manages all overlays for a single buffer.
#[derive(Clone)]
pub struct OverlayList {
    overlays: HashMap<u64, Overlay>,
    by_start: BTreeMap<usize, BTreeSet<u64>>,
    by_end: BTreeMap<usize, BTreeSet<u64>>,
    next_id: u64,
}

impl OverlayList {
    /// Create an empty overlay list.
    pub fn new() -> Self {
        Self {
            overlays: HashMap::new(),
            by_start: BTreeMap::new(),
            by_end: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Create a new overlay covering `[start, end)`.
    ///
    /// Returns the unique id assigned to the overlay. By default, both
    /// `front_advance` and `rear_advance` are `false`.
    pub fn make_overlay(&mut self, start: usize, end: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.insert_overlay(Overlay {
            id,
            start,
            end,
            properties: HashMap::new(),
            front_advance: false,
            rear_advance: false,
        });
        id
    }

    pub(crate) fn insert_overlay_with_id(&mut self, overlay: Overlay) {
        self.next_id = self.next_id.max(overlay.id.saturating_add(1));
        self.insert_overlay(overlay);
    }

    /// Delete the overlay with the given id.
    ///
    /// Returns `true` if the overlay was found and removed.
    pub fn delete_overlay(&mut self, id: u64) -> bool {
        let Some(overlay) = self.overlays.remove(&id) else {
            return false;
        };
        Self::remove_index_entry(&mut self.by_start, overlay.start, id);
        Self::remove_index_entry(&mut self.by_end, overlay.end, id);
        true
    }

    /// Set a property on an overlay.
    pub fn overlay_put(&mut self, id: u64, name: &str, value: Value) {
        if let Some(ov) = self.overlays.get_mut(&id) {
            ov.properties.insert(name.to_string(), value);
        }
    }

    /// Get a property from an overlay.
    pub fn overlay_get(&self, id: u64, name: &str) -> Option<&Value> {
        self.overlays
            .get(&id)
            .and_then(|ov| ov.properties.get(name))
    }

    /// Get the start position of an overlay.
    pub fn overlay_start(&self, id: u64) -> Option<usize> {
        self.overlays.get(&id).map(|ov| ov.start)
    }

    /// Get the end position of an overlay.
    pub fn overlay_end(&self, id: u64) -> Option<usize> {
        self.overlays.get(&id).map(|ov| ov.end)
    }

    /// Move an overlay to cover a new range `[start, end)`.
    pub fn move_overlay(&mut self, id: u64, start: usize, end: usize) {
        let Some((old_start, old_end)) = self.overlays.get(&id).map(|ov| (ov.start, ov.end)) else {
            return;
        };
        if let Some(ov) = self.overlays.get_mut(&id) {
            ov.start = start;
            ov.end = end;
        }
        Self::remove_index_entry(&mut self.by_start, old_start, id);
        Self::remove_index_entry(&mut self.by_end, old_end, id);
        Self::insert_index_entry(&mut self.by_start, start, id);
        Self::insert_index_entry(&mut self.by_end, end, id);
    }

    /// Return all overlay ids whose range covers position `pos`.
    ///
    /// An overlay covers `pos` if `overlay.start <= pos < overlay.end`.
    pub fn overlays_at(&self, pos: usize) -> Vec<u64> {
        let mut ids = Vec::new();
        for (_, start_ids) in self.by_start.range(..=pos) {
            for id in start_ids {
                let Some(overlay) = self.overlays.get(id) else {
                    continue;
                };
                if overlay.start <= pos && pos < overlay.end {
                    ids.push(*id);
                }
            }
        }
        ids
    }

    /// Return all overlay ids that overlap the range `[start, end)`.
    ///
    /// Matches GNU Emacs `overlays-in`: non-empty overlays overlap if they
    /// share at least one character with the region; empty overlays are
    /// included at BEG, between BEG and END, and at END only when END is the
    /// accessible end of the scanned buffer.
    pub fn overlays_in(&self, start: usize, end: usize) -> Vec<u64> {
        self.overlays_in_region(start, end, end)
    }

    pub fn overlays_in_region(&self, start: usize, end: usize, accessible_end: usize) -> Vec<u64> {
        let mut ids = Vec::new();
        for (_, start_ids) in self.by_start.range(..=end) {
            for id in start_ids {
                let Some(overlay) = self.overlays.get(id) else {
                    continue;
                };
                if overlay_overlaps_region(overlay, start, end, accessible_end) {
                    ids.push(*id);
                }
            }
        }
        ids
    }

    pub fn highest_priority_overlay_at(&self, pos: usize, property: &str) -> Option<u64> {
        self.best_overlay_for(property, |ov| ov.start <= pos && pos < ov.end)
    }

    pub fn highest_priority_overlay_for_inserted_char(
        &self,
        pos: usize,
        property: &str,
    ) -> Option<u64> {
        self.best_overlay_for(property, |ov| {
            !(ov.start == pos && ov.front_advance)
                && !(ov.end == pos && !ov.rear_advance)
                && ov.start <= pos
                && pos <= ov.end
        })
    }

    pub fn sort_overlay_ids_by_priority_desc(&self, overlay_ids: &mut [u64]) {
        overlay_ids.sort_by(
            |left_id, right_id| match (self.get(*left_id), self.get(*right_id)) {
                (Some(left), Some(right)) => compare_overlay_precedence(right, left),
                _ => Ordering::Equal,
            },
        );
    }

    /// Adjust all overlay positions after text is inserted at `pos` with
    /// `len` bytes.
    ///
    /// - If `front_advance` is true and insertion is at the start, the
    ///   start position advances.
    /// - If `rear_advance` is true and insertion is at the end, the end
    ///   position advances.
    pub fn adjust_for_insert(&mut self, pos: usize, len: usize) {
        if len == 0 {
            return;
        }
        for ov in self.overlays.values_mut() {
            // Adjust start.
            if ov.start > pos {
                ov.start += len;
            } else if ov.start == pos && ov.front_advance {
                ov.start += len;
            }

            // Adjust end.
            if ov.end > pos {
                ov.end += len;
            } else if ov.end == pos && ov.rear_advance {
                ov.end += len;
            }
        }
        self.rebuild_indexes();
    }

    /// Adjust all overlay positions after text in `[start, end)` is deleted.
    ///
    /// - Endpoints before the deleted range are unchanged.
    /// - Endpoints within the deleted range are clamped to `start`.
    /// - Endpoints after the deleted range are shifted left by `end - start`.
    /// - Overlays with the `evaporate` property that become zero-width are removed.
    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;

        for ov in self.overlays.values_mut() {
            // Adjust start.
            if ov.start >= end {
                ov.start -= len;
            } else if ov.start > start {
                ov.start = start;
            }

            // Adjust end.
            if ov.end >= end {
                ov.end -= len;
            } else if ov.end > start {
                ov.end = start;
            }
        }

        // GNU Emacs: overlays with the `evaporate` property are deleted when
        // they become empty (start == end) after text deletion.
        self.overlays.retain(|_, ov| {
            if ov.start == ov.end {
                if let Some(val) = ov.properties.get("evaporate") {
                    if val.is_truthy() {
                        return false; // remove this overlay
                    }
                }
            }
            true
        });
        self.rebuild_indexes();
    }

    /// Set the `front_advance` flag on an overlay.
    pub fn set_front_advance(&mut self, id: u64, advance: bool) {
        if let Some(ov) = self.overlays.get_mut(&id) {
            ov.front_advance = advance;
        }
    }

    /// Set the `rear_advance` flag on an overlay.
    pub fn set_rear_advance(&mut self, id: u64, advance: bool) {
        if let Some(ov) = self.overlays.get_mut(&id) {
            ov.rear_advance = advance;
        }
    }

    /// Return a reference to an overlay by id, if it exists.
    pub fn get(&self, id: u64) -> Option<&Overlay> {
        self.overlays.get(&id)
    }

    /// Return the number of overlays.
    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    /// Return true if there are no overlays.
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Remove all overlays that have a given property set.
    pub fn remove_overlays_by_property(&mut self, name: &str) {
        let ids: Vec<u64> = self
            .overlays
            .values()
            .filter(|ov| ov.properties.contains_key(name))
            .map(|ov| ov.id)
            .collect();
        for id in ids {
            let _ = self.delete_overlay(id);
        }
    }

    /// Return the smallest overlay start or end position that is strictly
    /// greater than `pos`.  Used by the layout engine to ensure `next_check`
    /// doesn't skip over overlay boundaries.
    pub fn next_boundary_after(&self, pos: usize) -> Option<usize> {
        let next_start = self
            .by_start
            .range((Excluded(pos), Unbounded))
            .next()
            .map(|(boundary, _)| *boundary);
        let next_end = self
            .by_end
            .range((Excluded(pos), Unbounded))
            .next()
            .map(|(boundary, _)| *boundary);
        match (next_start, next_end) {
            (Some(start), Some(end)) => Some(start.min(end)),
            (Some(start), None) => Some(start),
            (None, Some(end)) => Some(end),
            (None, None) => None,
        }
    }

    pub fn previous_boundary_before(&self, pos: usize) -> Option<usize> {
        let prev_start = self
            .by_start
            .range(..pos)
            .next_back()
            .map(|(boundary, _)| *boundary);
        let prev_end = self
            .by_end
            .range(..pos)
            .next_back()
            .map(|(boundary, _)| *boundary);
        match (prev_start, prev_end) {
            (Some(start), Some(end)) => Some(start.max(end)),
            (Some(start), None) => Some(start),
            (None, Some(end)) => Some(end),
            (None, None) => None,
        }
    }

    // pdump accessors
    pub(crate) fn dump_overlays(&self) -> Vec<Overlay> {
        let mut overlays: Vec<Overlay> = self.overlays.values().cloned().collect();
        overlays.sort_by_key(|overlay| overlay.id);
        overlays
    }
    pub(crate) fn dump_next_id(&self) -> u64 {
        self.next_id
    }
    pub(crate) fn from_dump(overlays: Vec<Overlay>, next_id: u64) -> Self {
        let mut list = Self {
            overlays: HashMap::new(),
            by_start: BTreeMap::new(),
            by_end: BTreeMap::new(),
            next_id,
        };
        for overlay in overlays {
            list.insert_overlay(overlay);
        }
        list
    }

    fn best_overlay_for<F>(&self, property: &str, predicate: F) -> Option<u64>
    where
        F: Fn(&Overlay) -> bool,
    {
        let mut best: Option<&Overlay> = None;
        for overlay in self.overlays.values() {
            if !predicate(overlay) {
                continue;
            }
            let Some(value) = overlay.properties.get(property) else {
                continue;
            };
            if value.is_nil() {
                continue;
            }
            match best {
                None => best = Some(overlay),
                Some(current) if compare_overlay_precedence(current, overlay) == Ordering::Less => {
                    best = Some(overlay);
                }
                _ => {}
            }
        }
        best.map(|overlay| overlay.id)
    }

    fn insert_overlay(&mut self, overlay: Overlay) {
        let id = overlay.id;
        let start = overlay.start;
        let end = overlay.end;
        self.overlays.insert(id, overlay);
        Self::insert_index_entry(&mut self.by_start, start, id);
        Self::insert_index_entry(&mut self.by_end, end, id);
    }

    fn insert_index_entry(index: &mut BTreeMap<usize, BTreeSet<u64>>, boundary: usize, id: u64) {
        index.entry(boundary).or_default().insert(id);
    }

    fn remove_index_entry(index: &mut BTreeMap<usize, BTreeSet<u64>>, boundary: usize, id: u64) {
        if let Some(ids) = index.get_mut(&boundary) {
            ids.remove(&id);
            if ids.is_empty() {
                index.remove(&boundary);
            }
        }
    }

    fn rebuild_indexes(&mut self) {
        self.by_start.clear();
        self.by_end.clear();
        let boundaries: Vec<(u64, usize, usize)> = self
            .overlays
            .values()
            .map(|overlay| (overlay.id, overlay.start, overlay.end))
            .collect();
        for (id, start, end) in boundaries {
            self.by_start.entry(start).or_default().insert(id);
            self.by_end.entry(end).or_default().insert(id);
        }
    }
}

fn overlay_overlaps_region(
    overlay: &Overlay,
    start: usize,
    end: usize,
    accessible_end: usize,
) -> bool {
    if overlay.start == overlay.end {
        return overlay.start == start
            || (start < overlay.start && overlay.start < end)
            || (overlay.start == end && end == accessible_end);
    }
    if start == end {
        return overlay.start <= start && start < overlay.end;
    }
    overlay.start < end && overlay.end > start
}

fn compare_overlay_precedence(left: &Overlay, right: &Overlay) -> Ordering {
    let (left_priority, left_subpriority) = overlay_priority(left);
    let (right_priority, right_subpriority) = overlay_priority(right);

    if left_priority != right_priority {
        return left_priority.cmp(&right_priority);
    }
    if left.start < right.start {
        if left.end < right.end && left_subpriority > right_subpriority {
            Ordering::Greater
        } else {
            Ordering::Less
        }
    } else if left.start > right.start {
        if left.end > right.end && left_subpriority < right_subpriority {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if left.end != right.end {
        if right.end < left.end {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if left_subpriority != right_subpriority {
        left_subpriority.cmp(&right_subpriority)
    } else if left.id == right.id {
        Ordering::Equal
    } else {
        left.id.cmp(&right.id)
    }
}

fn overlay_priority(overlay: &Overlay) -> (i64, i64) {
    match overlay.properties.get("priority") {
        None => (0, 0),
        Some(value) => match value {
            Value::Int(n) => (*n, 0),
            Value::Char(c) => (*c as i64, 0),
            Value::Cons(_) => (
                priority_component(value.cons_car()),
                priority_component(value.cons_cdr()),
            ),
            _ => (0, 0),
        },
    }
}

fn priority_component(value: Value) -> i64 {
    match value {
        Value::Int(n) => n,
        Value::Char(c) => c as i64,
        _ => 0,
    }
}

impl Default for OverlayList {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for OverlayList {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for overlay in self.overlays.values() {
            for value in overlay.properties.values() {
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
    // Basic creation and deletion
    // -----------------------------------------------------------------------

    #[test]
    fn new_overlay_list_is_empty() {
        let list = OverlayList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn make_overlay_returns_unique_ids() {
        let mut list = OverlayList::new();
        let id1 = list.make_overlay(0, 10);
        let id2 = list.make_overlay(5, 15);
        assert_ne!(id1, id2);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn delete_overlay_basic() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        assert!(list.delete_overlay(id));
        assert!(list.is_empty());
    }

    #[test]
    fn delete_nonexistent_overlay() {
        let mut list = OverlayList::new();
        list.make_overlay(0, 10);
        assert!(!list.delete_overlay(999));
        assert_eq!(list.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Properties
    // -----------------------------------------------------------------------

    #[test]
    fn overlay_put_and_get() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        list.overlay_put(id, "face", Value::symbol("bold"));

        let val = list.overlay_get(id, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "bold")
        );
    }

    #[test]
    fn overlay_get_nonexistent_property() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        assert!(list.overlay_get(id, "face").is_none());
    }

    #[test]
    fn overlay_get_nonexistent_id() {
        let list = OverlayList::new();
        assert!(list.overlay_get(999, "face").is_none());
    }

    #[test]
    fn overlay_put_overwrites() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        list.overlay_put(id, "face", Value::symbol("bold"));
        list.overlay_put(id, "face", Value::symbol("italic"));

        let val = list.overlay_get(id, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "italic")
        );
    }

    // -----------------------------------------------------------------------
    // Start / end / move
    // -----------------------------------------------------------------------

    #[test]
    fn overlay_start_end() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 15);
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(15));
    }

    #[test]
    fn overlay_start_end_nonexistent() {
        let list = OverlayList::new();
        assert_eq!(list.overlay_start(999), None);
        assert_eq!(list.overlay_end(999), None);
    }

    #[test]
    fn move_overlay_basic() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        list.move_overlay(id, 20, 30);
        assert_eq!(list.overlay_start(id), Some(20));
        assert_eq!(list.overlay_end(id), Some(30));
    }

    #[test]
    fn move_preserves_properties() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(0, 10);
        list.overlay_put(id, "face", Value::symbol("bold"));
        list.move_overlay(id, 20, 30);

        let val = list.overlay_get(id, "face").unwrap();
        assert!(
            matches!(val, Value::Symbol(id) if crate::emacs_core::intern::resolve_sym(*id) == "bold")
        );
    }

    // -----------------------------------------------------------------------
    // overlays_at / overlays_in
    // -----------------------------------------------------------------------

    #[test]
    fn overlays_at_basic() {
        let mut list = OverlayList::new();
        let id1 = list.make_overlay(0, 10);
        let id2 = list.make_overlay(5, 15);
        let _id3 = list.make_overlay(20, 30);

        let at_7 = list.overlays_at(7);
        assert!(at_7.contains(&id1));
        assert!(at_7.contains(&id2));
        assert_eq!(at_7.len(), 2);
    }

    #[test]
    fn overlays_at_boundary() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);

        // Start boundary: included
        assert!(list.overlays_at(5).contains(&id));
        // End boundary: excluded
        assert!(!list.overlays_at(10).contains(&id));
    }

    #[test]
    fn overlays_at_outside() {
        let mut list = OverlayList::new();
        list.make_overlay(5, 10);
        assert!(list.overlays_at(3).is_empty());
        assert!(list.overlays_at(15).is_empty());
    }

    #[test]
    fn overlays_in_basic() {
        let mut list = OverlayList::new();
        let id1 = list.make_overlay(0, 10);
        let id2 = list.make_overlay(5, 15);
        let id3 = list.make_overlay(20, 30);

        let in_range = list.overlays_in(3, 12);
        assert!(in_range.contains(&id1));
        assert!(in_range.contains(&id2));
        assert!(!in_range.contains(&id3));
    }

    #[test]
    fn overlays_in_empty_range() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        // Range [7, 7) has zero width. Following Emacs semantics,
        // overlays-in with start == end still finds overlays at that point
        // because the filter is overlay.start < end && overlay.end > start,
        // which is 5 < 7 && 10 > 7 => true.
        assert!(list.overlays_in(7, 7).contains(&id));
        // But a zero-width range outside all overlays should be empty.
        assert!(list.overlays_in(15, 15).is_empty());
    }

    #[test]
    fn overlays_in_exact_match() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        assert!(list.overlays_in(5, 10).contains(&id));
    }

    #[test]
    fn overlays_in_touching_but_not_overlapping() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        // Range ends exactly where overlay starts: no overlap
        assert!(!list.overlays_in(0, 5).contains(&id));
        // Range starts exactly where overlay ends: no overlap
        assert!(!list.overlays_in(10, 15).contains(&id));
    }

    // -----------------------------------------------------------------------
    // adjust_for_insert
    // -----------------------------------------------------------------------

    #[test]
    fn adjust_insert_shifts_after() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(10, 20);

        list.adjust_for_insert(5, 3);

        assert_eq!(list.overlay_start(id), Some(13));
        assert_eq!(list.overlay_end(id), Some(23));
    }

    #[test]
    fn adjust_insert_expands_spanning() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 15);

        list.adjust_for_insert(10, 3);

        // Start unchanged, end shifts
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(18));
    }

    #[test]
    fn adjust_insert_before_unchanged() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);

        list.adjust_for_insert(15, 3);

        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    #[test]
    fn adjust_insert_at_start_no_front_advance() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        // front_advance = false (default)

        list.adjust_for_insert(5, 3);

        // Start stays, end shifts
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(13));
    }

    #[test]
    fn adjust_insert_at_start_with_front_advance() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        list.set_front_advance(id, true);

        list.adjust_for_insert(5, 3);

        // Both start and end shift
        assert_eq!(list.overlay_start(id), Some(8));
        assert_eq!(list.overlay_end(id), Some(13));
    }

    #[test]
    fn adjust_insert_at_end_no_rear_advance() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        // rear_advance = false (default)

        list.adjust_for_insert(10, 3);

        // Neither start nor end changes
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    #[test]
    fn adjust_insert_at_end_with_rear_advance() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        list.set_rear_advance(id, true);

        list.adjust_for_insert(10, 3);

        // End advances
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(13));
    }

    #[test]
    fn adjust_insert_zero_length() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);

        list.adjust_for_insert(7, 0);

        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    // -----------------------------------------------------------------------
    // adjust_for_delete
    // -----------------------------------------------------------------------

    #[test]
    fn adjust_delete_shifts_after() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(10, 20);

        list.adjust_for_delete(2, 5);

        // Shifted left by 3
        assert_eq!(list.overlay_start(id), Some(7));
        assert_eq!(list.overlay_end(id), Some(17));
    }

    #[test]
    fn adjust_delete_clamps_inside() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 15);

        list.adjust_for_delete(3, 20);

        // Both endpoints clamped to 3 (deletion encompasses entire overlay)
        assert_eq!(list.overlay_start(id), Some(3));
        assert_eq!(list.overlay_end(id), Some(3));
    }

    #[test]
    fn adjust_delete_shrinks_spanning() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 20);

        list.adjust_for_delete(10, 15);

        // Start unchanged, end shifts left by 5
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(15));
    }

    #[test]
    fn adjust_delete_truncates_end() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 15);

        list.adjust_for_delete(10, 20);

        // End clamped to start of deletion, deletion goes past end
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    #[test]
    fn adjust_delete_truncates_start() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 15);

        list.adjust_for_delete(2, 10);

        // Start clamped to 2, end shifts left by 8
        assert_eq!(list.overlay_start(id), Some(2));
        assert_eq!(list.overlay_end(id), Some(7));
    }

    #[test]
    fn adjust_delete_before_unchanged() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);

        list.adjust_for_delete(15, 20);

        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    #[test]
    fn adjust_delete_empty_range() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);

        list.adjust_for_delete(7, 7);

        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(10));
    }

    // -----------------------------------------------------------------------
    // Multiple overlays
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_overlays_adjust_correctly() {
        let mut list = OverlayList::new();
        let id1 = list.make_overlay(0, 10);
        let id2 = list.make_overlay(5, 15);
        let id3 = list.make_overlay(20, 30);

        // Insert 5 bytes at position 12
        list.adjust_for_insert(12, 5);

        // id1: [0, 10) — before insertion, unchanged
        assert_eq!(list.overlay_start(id1), Some(0));
        assert_eq!(list.overlay_end(id1), Some(10));

        // id2: [5, 15) — end past insertion, end shifts
        assert_eq!(list.overlay_start(id2), Some(5));
        assert_eq!(list.overlay_end(id2), Some(20));

        // id3: [20, 30) — entirely after, both shift
        assert_eq!(list.overlay_start(id3), Some(25));
        assert_eq!(list.overlay_end(id3), Some(35));
    }

    #[test]
    fn get_overlay_by_id() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 10);
        list.overlay_put(id, "face", Value::symbol("bold"));

        let ov = list.get(id).unwrap();
        assert_eq!(ov.start, 5);
        assert_eq!(ov.end, 10);
        assert!(ov.properties.contains_key("face"));
    }

    #[test]
    fn get_nonexistent_overlay() {
        let list = OverlayList::new();
        assert!(list.get(999).is_none());
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn zero_width_overlay() {
        let mut list = OverlayList::new();
        let id = list.make_overlay(5, 5);

        // Zero-width overlay: start == end, so it doesn't cover any position
        assert!(list.overlays_at(5).is_empty());
        assert_eq!(list.overlay_start(id), Some(5));
        assert_eq!(list.overlay_end(id), Some(5));
    }

    #[test]
    fn delete_overlay_preserves_others() {
        let mut list = OverlayList::new();
        let id1 = list.make_overlay(0, 10);
        let id2 = list.make_overlay(5, 15);
        let id3 = list.make_overlay(20, 30);

        list.delete_overlay(id2);

        assert_eq!(list.len(), 2);
        assert!(list.get(id1).is_some());
        assert!(list.get(id2).is_none());
        assert!(list.get(id3).is_some());
    }

    #[test]
    fn default_creates_empty_list() {
        let list = OverlayList::default();
        assert!(list.is_empty());
    }

    #[test]
    fn overlays_in_region_includes_empty_overlay_at_query_start() {
        let mut list = OverlayList::new();
        let empty = list.make_overlay(7, 7);
        let spanning = list.make_overlay(7, 10);

        let ids = list.overlays_in_region(7, 7, 10);
        assert!(ids.contains(&empty));
        assert!(ids.contains(&spanning));
    }

    #[test]
    fn previous_boundary_before_uses_indexed_boundaries() {
        let mut list = OverlayList::new();
        list.make_overlay(3, 8);
        list.make_overlay(10, 12);

        assert_eq!(list.previous_boundary_before(10), Some(8));
        assert_eq!(list.previous_boundary_before(3), None);
        assert_eq!(list.next_boundary_after(8), Some(10));
    }
}
