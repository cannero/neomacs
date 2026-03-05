//! Overlay system for buffers.
//!
//! Overlays are like text properties but are attached to the buffer rather
//! than the text itself. Each overlay has a start and end position, a set
//! of properties, and flags controlling whether its endpoints advance when
//! text is inserted at the boundary.

use std::collections::HashMap;

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
    overlays: Vec<Overlay>,
    next_id: u64,
}

impl OverlayList {
    /// Create an empty overlay list.
    pub fn new() -> Self {
        Self {
            overlays: Vec::new(),
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
        self.overlays.push(Overlay {
            id,
            start,
            end,
            properties: HashMap::new(),
            front_advance: false,
            rear_advance: false,
        });
        id
    }

    /// Delete the overlay with the given id.
    ///
    /// Returns `true` if the overlay was found and removed.
    pub fn delete_overlay(&mut self, id: u64) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|ov| ov.id != id);
        self.overlays.len() < before
    }

    /// Set a property on an overlay.
    pub fn overlay_put(&mut self, id: u64, name: &str, value: Value) {
        if let Some(ov) = self.overlays.iter_mut().find(|ov| ov.id == id) {
            ov.properties.insert(name.to_string(), value);
        }
    }

    /// Get a property from an overlay.
    pub fn overlay_get(&self, id: u64, name: &str) -> Option<&Value> {
        self.overlays
            .iter()
            .find(|ov| ov.id == id)
            .and_then(|ov| ov.properties.get(name))
    }

    /// Get the start position of an overlay.
    pub fn overlay_start(&self, id: u64) -> Option<usize> {
        self.overlays
            .iter()
            .find(|ov| ov.id == id)
            .map(|ov| ov.start)
    }

    /// Get the end position of an overlay.
    pub fn overlay_end(&self, id: u64) -> Option<usize> {
        self.overlays.iter().find(|ov| ov.id == id).map(|ov| ov.end)
    }

    /// Move an overlay to cover a new range `[start, end)`.
    pub fn move_overlay(&mut self, id: u64, start: usize, end: usize) {
        if let Some(ov) = self.overlays.iter_mut().find(|ov| ov.id == id) {
            ov.start = start;
            ov.end = end;
        }
    }

    /// Return all overlay ids whose range covers position `pos`.
    ///
    /// An overlay covers `pos` if `overlay.start <= pos < overlay.end`.
    pub fn overlays_at(&self, pos: usize) -> Vec<u64> {
        self.overlays
            .iter()
            .filter(|ov| ov.start <= pos && pos < ov.end)
            .map(|ov| ov.id)
            .collect()
    }

    /// Return all overlay ids that overlap the range `[start, end)`.
    ///
    /// An overlay overlaps if `overlay.start < end && overlay.end > start`.
    pub fn overlays_in(&self, start: usize, end: usize) -> Vec<u64> {
        self.overlays
            .iter()
            .filter(|ov| ov.start < end && ov.end > start)
            .map(|ov| ov.id)
            .collect()
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
        for ov in &mut self.overlays {
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
    }

    /// Adjust all overlay positions after text in `[start, end)` is deleted.
    ///
    /// - Endpoints before the deleted range are unchanged.
    /// - Endpoints within the deleted range are clamped to `start`.
    /// - Endpoints after the deleted range are shifted left by `end - start`.
    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;

        for ov in &mut self.overlays {
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
    }

    /// Set the `front_advance` flag on an overlay.
    pub fn set_front_advance(&mut self, id: u64, advance: bool) {
        if let Some(ov) = self.overlays.iter_mut().find(|ov| ov.id == id) {
            ov.front_advance = advance;
        }
    }

    /// Set the `rear_advance` flag on an overlay.
    pub fn set_rear_advance(&mut self, id: u64, advance: bool) {
        if let Some(ov) = self.overlays.iter_mut().find(|ov| ov.id == id) {
            ov.rear_advance = advance;
        }
    }

    /// Return a reference to an overlay by id, if it exists.
    pub fn get(&self, id: u64) -> Option<&Overlay> {
        self.overlays.iter().find(|ov| ov.id == id)
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
        self.overlays.retain(|ov| !ov.properties.contains_key(name));
    }

    /// Return the smallest overlay start or end position that is strictly
    /// greater than `pos`.  Used by the layout engine to ensure `next_check`
    /// doesn't skip over overlay boundaries.
    pub fn next_boundary_after(&self, pos: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        for ov in &self.overlays {
            if ov.start > pos {
                best = Some(best.map_or(ov.start, |b: usize| b.min(ov.start)));
            }
            if ov.end > pos {
                best = Some(best.map_or(ov.end, |b: usize| b.min(ov.end)));
            }
        }
        best
    }

    // pdump accessors
    pub(crate) fn dump_overlays(&self) -> &[Overlay] { &self.overlays }
    pub(crate) fn dump_next_id(&self) -> u64 { self.next_id }
    pub(crate) fn from_dump(overlays: Vec<Overlay>, next_id: u64) -> Self { Self { overlays, next_id } }
}

impl Default for OverlayList {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for OverlayList {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for overlay in &self.overlays {
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
}
