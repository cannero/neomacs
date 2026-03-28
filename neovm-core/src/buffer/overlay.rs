//! Overlay system for buffers.
//!
//! GNU Emacs exposes overlays as first-class Lisp objects whose identity
//! outlives deletion. The buffer owns the interval index, while the overlay
//! object owns plist, buffer membership, and endpoint state. NeoVM models that
//! split by keeping overlay objects on the GC heap and storing only live object
//! ids in each buffer's overlay index.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

use crate::emacs_core::value::{Value, eq_value, read_cons, with_heap, with_heap_mut};
use crate::gc::GcTrace;
use crate::gc::types::{ObjId, OverlayData};

pub type Overlay = OverlayData;

#[derive(Clone)]
pub struct OverlayList {
    overlays: BTreeSet<ObjId>,
    by_start: BTreeMap<usize, BTreeSet<ObjId>>,
    by_end: BTreeMap<usize, BTreeSet<ObjId>>,
}

impl OverlayList {
    pub fn new() -> Self {
        Self {
            overlays: BTreeSet::new(),
            by_start: BTreeMap::new(),
            by_end: BTreeMap::new(),
        }
    }

    pub fn insert_overlay(&mut self, overlay: ObjId) {
        let data = with_heap(|h| h.get_overlay(overlay).clone());
        self.overlays.insert(overlay);
        Self::insert_index_entry(&mut self.by_start, data.start, overlay);
        Self::insert_index_entry(&mut self.by_end, data.end, overlay);
    }

    pub fn detach_overlay(&mut self, overlay: ObjId) -> bool {
        if !self.overlays.remove(&overlay) {
            return false;
        }
        if let Some((start, end)) = overlay_range(overlay) {
            Self::remove_index_entry(&mut self.by_start, start, overlay);
            Self::remove_index_entry(&mut self.by_end, end, overlay);
        }
        true
    }

    pub fn delete_overlay(&mut self, overlay: ObjId) -> bool {
        if !self.detach_overlay(overlay) {
            return false;
        }
        with_heap_mut(|h| {
            let object = h.get_overlay_mut(overlay);
            object.buffer = None;
        });
        true
    }

    pub fn overlay_put(&mut self, overlay: ObjId, prop: Value, value: Value) -> bool {
        let changed = with_heap_mut(|h| {
            let object = h.get_overlay_mut(overlay);
            let (plist, changed) = plist_put_eq(object.plist, prop, value);
            object.plist = plist;
            changed
        });
        changed
    }

    pub fn overlay_get(&self, overlay: ObjId, prop: &Value) -> Option<Value> {
        with_heap(|h| plist_get_eq(h.get_overlay(overlay).plist, prop))
    }

    pub fn overlay_get_named(&self, overlay: ObjId, prop_name: &str) -> Option<Value> {
        overlay_property_named(overlay, prop_name)
    }

    pub fn overlay_plist(&self, overlay: ObjId) -> Option<Value> {
        if self.overlays.contains(&overlay) || overlay_live_buffer(overlay).is_none() {
            return Some(with_heap(|h| h.get_overlay(overlay).plist));
        }
        None
    }

    pub fn overlay_start(&self, overlay: ObjId) -> Option<usize> {
        if overlay_live_buffer(overlay).is_none() {
            return None;
        }
        overlay_range(overlay).map(|(start, _)| start)
    }

    pub fn overlay_end(&self, overlay: ObjId) -> Option<usize> {
        if overlay_live_buffer(overlay).is_none() {
            return None;
        }
        overlay_range(overlay).map(|(_, end)| end)
    }

    pub fn move_overlay(&mut self, overlay: ObjId, start: usize, end: usize) {
        let Some((old_start, old_end)) = overlay_range(overlay) else {
            return;
        };
        with_heap_mut(|h| {
            let object = h.get_overlay_mut(overlay);
            object.start = start;
            object.end = end;
        });
        Self::remove_index_entry(&mut self.by_start, old_start, overlay);
        Self::remove_index_entry(&mut self.by_end, old_end, overlay);
        Self::insert_index_entry(&mut self.by_start, start, overlay);
        Self::insert_index_entry(&mut self.by_end, end, overlay);
    }

    pub fn overlays_at(&self, pos: usize) -> Vec<ObjId> {
        let mut overlays = Vec::new();
        for (_, ids) in self.by_start.range(..=pos) {
            for overlay in ids {
                if overlay_covers_pos(*overlay, pos) {
                    overlays.push(*overlay);
                }
            }
        }
        overlays
    }

    pub fn overlays_in(&self, start: usize, end: usize) -> Vec<ObjId> {
        self.overlays_in_region(start, end, end)
    }

    pub fn overlays_in_region(
        &self,
        start: usize,
        end: usize,
        accessible_end: usize,
    ) -> Vec<ObjId> {
        let mut overlays = Vec::new();
        for (_, ids) in self.by_start.range(..=end) {
            for overlay in ids {
                if overlay_overlaps_region(*overlay, start, end, accessible_end) {
                    overlays.push(*overlay);
                }
            }
        }
        overlays
    }

    pub fn highest_priority_overlay_at(&self, pos: usize, property: &str) -> Option<ObjId> {
        self.best_overlay_for(property, |overlay| overlay_covers_pos(overlay, pos))
    }

    pub fn highest_priority_overlay_for_inserted_char(
        &self,
        pos: usize,
        property: &str,
    ) -> Option<ObjId> {
        self.best_overlay_for(property, |overlay| {
            let Some(snapshot) = overlay_snapshot(overlay) else {
                return false;
            };
            !(snapshot.start == pos && snapshot.front_advance)
                && !(snapshot.end == pos && !snapshot.rear_advance)
                && snapshot.start <= pos
                && pos <= snapshot.end
        })
    }

    pub fn sort_overlay_ids_by_priority_desc(&self, overlay_ids: &mut [ObjId]) {
        overlay_ids.sort_by(|left, right| compare_overlay_precedence(*right, *left));
    }

    pub fn adjust_for_insert(&mut self, pos: usize, len: usize) {
        if len == 0 {
            return;
        }
        let live: Vec<ObjId> = self.overlays.iter().copied().collect();
        with_heap_mut(|h| {
            for overlay in &live {
                let object = h.get_overlay_mut(*overlay);
                if object.start > pos {
                    object.start += len;
                } else if object.start == pos && object.front_advance {
                    object.start += len;
                }

                if object.end > pos {
                    object.end += len;
                } else if object.end == pos && object.rear_advance {
                    object.end += len;
                }
            }
        });
        self.rebuild_indexes();
    }

    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;
        let live: Vec<ObjId> = self.overlays.iter().copied().collect();
        let mut evaporated = Vec::new();
        with_heap_mut(|h| {
            for overlay in &live {
                let object = h.get_overlay_mut(*overlay);
                if object.start >= end {
                    object.start -= len;
                } else if object.start > start {
                    object.start = start;
                }

                if object.end >= end {
                    object.end -= len;
                } else if object.end > start {
                    object.end = start;
                }

                if object.start == object.end
                    && plist_get_eq(object.plist, &Value::symbol("evaporate"))
                        .is_some_and(|v| v.is_truthy())
                {
                    object.buffer = None;
                    evaporated.push(*overlay);
                }
            }
        });

        for overlay in evaporated {
            self.overlays.remove(&overlay);
        }
        self.rebuild_indexes();
    }

    pub fn set_front_advance(&mut self, overlay: ObjId, advance: bool) {
        with_heap_mut(|h| {
            h.get_overlay_mut(overlay).front_advance = advance;
        });
    }

    pub fn set_rear_advance(&mut self, overlay: ObjId, advance: bool) {
        with_heap_mut(|h| {
            h.get_overlay_mut(overlay).rear_advance = advance;
        });
    }

    pub fn get(&self, overlay: ObjId) -> Option<Overlay> {
        self.overlays
            .contains(&overlay)
            .then(|| with_heap(|h| h.get_overlay(overlay).clone()))
    }

    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

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

    pub(crate) fn dump_overlays(&self) -> Vec<ObjId> {
        self.overlays.iter().copied().collect()
    }

    pub(crate) fn from_dump(overlays: Vec<ObjId>) -> Self {
        let mut list = Self::new();
        for overlay in overlays {
            if overlay_live_buffer(overlay).is_some() {
                list.insert_overlay(overlay);
            }
        }
        list
    }

    fn best_overlay_for<F>(&self, property: &str, predicate: F) -> Option<ObjId>
    where
        F: Fn(ObjId) -> bool,
    {
        let mut best: Option<ObjId> = None;
        for overlay in &self.overlays {
            if !predicate(*overlay) {
                continue;
            }
            let Some(value) = overlay_property_named(*overlay, property) else {
                continue;
            };
            if value.is_nil() {
                continue;
            }
            match best {
                None => best = Some(*overlay),
                Some(current)
                    if compare_overlay_precedence(current, *overlay) == Ordering::Less =>
                {
                    best = Some(*overlay);
                }
                _ => {}
            }
        }
        best
    }

    fn insert_index_entry(
        index: &mut BTreeMap<usize, BTreeSet<ObjId>>,
        boundary: usize,
        overlay: ObjId,
    ) {
        index.entry(boundary).or_default().insert(overlay);
    }

    fn remove_index_entry(
        index: &mut BTreeMap<usize, BTreeSet<ObjId>>,
        boundary: usize,
        overlay: ObjId,
    ) {
        if let Some(ids) = index.get_mut(&boundary) {
            ids.remove(&overlay);
            if ids.is_empty() {
                index.remove(&boundary);
            }
        }
    }

    fn rebuild_indexes(&mut self) {
        self.by_start.clear();
        self.by_end.clear();
        let live: Vec<ObjId> = self.overlays.iter().copied().collect();
        for overlay in live {
            if overlay_live_buffer(overlay).is_none() {
                self.overlays.remove(&overlay);
                continue;
            }
            if let Some((start, end)) = overlay_range(overlay) {
                Self::insert_index_entry(&mut self.by_start, start, overlay);
                Self::insert_index_entry(&mut self.by_end, end, overlay);
            }
        }
    }
}

fn overlay_snapshot(overlay: ObjId) -> Option<Overlay> {
    let snapshot = with_heap(|h| h.get_overlay(overlay).clone());
    snapshot.buffer.map(|_| snapshot)
}

fn overlay_live_buffer(overlay: ObjId) -> Option<crate::buffer::BufferId> {
    with_heap(|h| h.get_overlay(overlay).buffer)
}

fn overlay_range(overlay: ObjId) -> Option<(usize, usize)> {
    overlay_snapshot(overlay).map(|overlay| (overlay.start, overlay.end))
}

fn overlay_covers_pos(overlay: ObjId, pos: usize) -> bool {
    let Some(snapshot) = overlay_snapshot(overlay) else {
        return false;
    };
    snapshot.start <= pos && pos < snapshot.end
}

fn overlay_overlaps_region(
    overlay: ObjId,
    start: usize,
    end: usize,
    accessible_end: usize,
) -> bool {
    let Some(snapshot) = overlay_snapshot(overlay) else {
        return false;
    };
    if snapshot.start == snapshot.end {
        return snapshot.start == start
            || (start < snapshot.start && snapshot.start < end)
            || (snapshot.start == end && end == accessible_end);
    }
    if start == end {
        return snapshot.start <= start && start < snapshot.end;
    }
    snapshot.start < end && snapshot.end > start
}

fn overlay_property_named(overlay: ObjId, prop_name: &str) -> Option<Value> {
    with_heap(|h| {
        let plist = h.get_overlay(overlay).plist;
        plist_get_named(plist, prop_name)
    })
}

fn compare_overlay_precedence(left: ObjId, right: ObjId) -> Ordering {
    let Some(left_overlay) = overlay_snapshot(left) else {
        return Ordering::Less;
    };
    let Some(right_overlay) = overlay_snapshot(right) else {
        return Ordering::Greater;
    };
    let (left_priority, left_subpriority) = overlay_priority(&left_overlay);
    let (right_priority, right_subpriority) = overlay_priority(&right_overlay);

    if left_priority != right_priority {
        return left_priority.cmp(&right_priority);
    }
    if left_overlay.start < right_overlay.start {
        if left_overlay.end < right_overlay.end && left_subpriority > right_subpriority {
            Ordering::Greater
        } else {
            Ordering::Less
        }
    } else if left_overlay.start > right_overlay.start {
        if left_overlay.end > right_overlay.end && left_subpriority < right_subpriority {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if left_overlay.end != right_overlay.end {
        if right_overlay.end < left_overlay.end {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if left_subpriority != right_subpriority {
        left_subpriority.cmp(&right_subpriority)
    } else {
        left.cmp(&right)
    }
}

fn overlay_priority(overlay: &Overlay) -> (i64, i64) {
    match plist_get_named(overlay.plist, "priority") {
        None => (0, 0),
        Some(value) => match value {
            Value::Int(n) => (n, 0),
            Value::Char(c) => (c as i64, 0),
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

pub(crate) fn plist_get_eq(plist: Value, prop: &Value) -> Option<Value> {
    let mut tail = plist;
    loop {
        let Value::Cons(cell) = tail else {
            return None;
        };
        let pair = read_cons(cell);
        let Value::Cons(value_cell) = pair.cdr else {
            return None;
        };
        if eq_value(&pair.car, prop) {
            return Some(with_heap(|h| h.cons_car(value_cell)));
        }
        tail = with_heap(|h| h.cons_cdr(value_cell));
    }
}

fn plist_get_named(plist: Value, prop_name: &str) -> Option<Value> {
    let mut tail = plist;
    loop {
        let Value::Cons(cell) = tail else {
            return None;
        };
        let pair = read_cons(cell);
        let Value::Cons(value_cell) = pair.cdr else {
            return None;
        };
        if pair.car.as_symbol_name() == Some(prop_name) {
            return Some(with_heap(|h| h.cons_car(value_cell)));
        }
        tail = with_heap(|h| h.cons_cdr(value_cell));
    }
}

pub(crate) fn plist_put_eq(plist: Value, prop: Value, value: Value) -> (Value, bool) {
    let mut tail = plist;
    loop {
        let Value::Cons(cell) = tail else {
            let changed = !value.is_nil();
            return (Value::cons(prop, Value::cons(value, plist)), changed);
        };
        let pair = read_cons(cell);
        let Value::Cons(value_cell) = pair.cdr else {
            let changed = !value.is_nil();
            return (Value::cons(prop, Value::cons(value, plist)), changed);
        };
        if eq_value(&pair.car, &prop) {
            let changed = with_heap(|h| !eq_value(&h.cons_car(value_cell), &value));
            with_heap_mut(|h| h.set_car(value_cell, value));
            return (plist, changed);
        }
        tail = with_heap(|h| h.cons_cdr(value_cell));
    }
}

impl Default for OverlayList {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for OverlayList {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for overlay in &self.overlays {
            roots.push(Value::Overlay(*overlay));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferId;
    use crate::emacs_core::value::with_heap_mut;

    fn alloc_overlay(start: usize, end: usize) -> ObjId {
        with_heap_mut(|h| {
            h.alloc_overlay(OverlayData {
                plist: Value::Nil,
                buffer: Some(BufferId(1)),
                start,
                end,
                front_advance: false,
                rear_advance: false,
            })
        })
    }

    #[test]
    fn insert_and_delete_overlay_preserves_object_identity() {
        let mut list = OverlayList::new();
        let overlay = alloc_overlay(2, 5);
        list.insert_overlay(overlay);
        assert_eq!(list.overlays_at(3), vec![overlay]);
        assert!(list.delete_overlay(overlay));
        assert!(list.overlays_at(3).is_empty());
        assert!(overlay_live_buffer(overlay).is_none());
    }

    #[test]
    fn overlay_put_preserves_existing_property_position() {
        let mut list = OverlayList::new();
        let overlay = alloc_overlay(0, 1);
        list.insert_overlay(overlay);
        let face = Value::symbol("face");
        let help = Value::symbol("help-echo");
        list.overlay_put(overlay, face, Value::symbol("bold"));
        list.overlay_put(overlay, help, Value::string("tip"));
        list.overlay_put(overlay, face, Value::symbol("italic"));
        let plist = list.overlay_plist(overlay).unwrap();
        assert_eq!(
            crate::emacs_core::print::print_value(&plist),
            "(help-echo \"tip\" face italic)"
        );
    }

    #[test]
    fn move_overlay_updates_boundaries() {
        let mut list = OverlayList::new();
        let overlay = alloc_overlay(0, 2);
        list.insert_overlay(overlay);
        list.move_overlay(overlay, 4, 7);
        assert_eq!(list.overlay_start(overlay), Some(4));
        assert_eq!(list.overlay_end(overlay), Some(7));
        assert_eq!(list.overlays_at(5), vec![overlay]);
    }

    #[test]
    fn insert_adjusts_front_and_rear_advance() {
        let mut list = OverlayList::new();
        let overlay = alloc_overlay(5, 10);
        list.insert_overlay(overlay);
        list.set_front_advance(overlay, true);
        list.set_rear_advance(overlay, true);
        list.adjust_for_insert(5, 2);
        assert_eq!(list.overlay_start(overlay), Some(7));
        assert_eq!(list.overlay_end(overlay), Some(12));
    }

    #[test]
    fn delete_evaporates_zero_width_overlay() {
        let mut list = OverlayList::new();
        let overlay = alloc_overlay(5, 10);
        list.insert_overlay(overlay);
        list.overlay_put(overlay, Value::symbol("evaporate"), Value::True);
        list.adjust_for_delete(5, 10);
        assert!(list.is_empty());
        assert!(overlay_live_buffer(overlay).is_none());
    }

    #[test]
    fn priority_sort_uses_gnu_precedence_rules() {
        let mut list = OverlayList::new();
        let low = alloc_overlay(2, 7);
        let high = alloc_overlay(4, 7);
        list.insert_overlay(low);
        list.insert_overlay(high);
        list.overlay_put(low, Value::symbol("face"), Value::symbol("bold"));
        list.overlay_put(low, Value::symbol("priority"), Value::Int(1));
        list.overlay_put(high, Value::symbol("face"), Value::symbol("italic"));
        list.overlay_put(
            high,
            Value::symbol("priority"),
            Value::cons(Value::Int(1), Value::Int(2)),
        );
        let mut ids = list.overlays_at(4);
        list.sort_overlay_ids_by_priority_desc(&mut ids);
        assert_eq!(ids, vec![high, low]);
    }
}
