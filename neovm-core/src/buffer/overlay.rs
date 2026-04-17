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

use crate::emacs_core::plist;
use crate::emacs_core::value::{Value, ValueKind};
use crate::gc_trace::GcTrace;
use crate::heap_types::OverlayData;

pub type Overlay = OverlayData;

#[derive(Clone)]
pub struct OverlayList {
    overlays: BTreeSet<Value>,
    by_start: BTreeMap<usize, BTreeSet<Value>>,
    by_end: BTreeMap<usize, BTreeSet<Value>>,
}

impl OverlayList {
    pub fn new() -> Self {
        Self {
            overlays: BTreeSet::new(),
            by_start: BTreeMap::new(),
            by_end: BTreeMap::new(),
        }
    }

    pub fn insert_overlay(&mut self, overlay: Value) {
        let data = overlay.as_overlay_data().unwrap();
        let start = data.start;
        let end = data.end;
        self.overlays.insert(overlay);
        Self::insert_index_entry(&mut self.by_start, start, overlay);
        Self::insert_index_entry(&mut self.by_end, end, overlay);
    }

    pub fn detach_overlay(&mut self, overlay: Value) -> bool {
        if !self.overlays.remove(&overlay) {
            return false;
        }
        if let Some((start, end)) = overlay_range(overlay) {
            Self::remove_index_entry(&mut self.by_start, start, overlay);
            Self::remove_index_entry(&mut self.by_end, end, overlay);
        }
        true
    }

    pub fn delete_overlay(&mut self, overlay: Value) -> bool {
        if !self.detach_overlay(overlay) {
            return false;
        }
        let _ = overlay.with_overlay_data_mut(|data| {
            data.buffer = None;
        });
        true
    }

    pub fn overlay_put(&mut self, overlay: Value, prop: Value, value: Value) -> bool {
        overlay
            .with_overlay_data_mut(|data| {
                let (plist, changed) = plist::plist_put(data.plist, prop, value);
                data.plist = plist;
                changed
            })
            .unwrap()
    }

    pub fn overlay_get(&self, overlay: Value, prop: &Value) -> Option<Value> {
        plist::plist_get(overlay.as_overlay_data().unwrap().plist, prop)
    }

    pub fn overlay_get_named(&self, overlay: Value, prop_name: Value) -> Option<Value> {
        overlay_property_named(overlay, prop_name)
    }

    pub fn overlay_plist(&self, overlay: Value) -> Option<Value> {
        if self.overlays.contains(&overlay) || overlay_live_buffer(overlay).is_none() {
            return Some(overlay.as_overlay_data().unwrap().plist);
        }
        None
    }

    pub fn overlay_start(&self, overlay: Value) -> Option<usize> {
        if overlay_live_buffer(overlay).is_none() {
            return None;
        }
        overlay_range(overlay).map(|(start, _)| start)
    }

    pub fn overlay_end(&self, overlay: Value) -> Option<usize> {
        if overlay_live_buffer(overlay).is_none() {
            return None;
        }
        overlay_range(overlay).map(|(_, end)| end)
    }

    pub fn move_overlay(&mut self, overlay: Value, start: usize, end: usize) {
        let Some((old_start, old_end)) = overlay_range(overlay) else {
            return;
        };
        let _ = overlay.with_overlay_data_mut(|data| {
            data.start = start;
            data.end = end;
        });
        Self::remove_index_entry(&mut self.by_start, old_start, overlay);
        Self::remove_index_entry(&mut self.by_end, old_end, overlay);
        Self::insert_index_entry(&mut self.by_start, start, overlay);
        Self::insert_index_entry(&mut self.by_end, end, overlay);
    }

    pub fn overlays_at(&self, pos: usize) -> Vec<Value> {
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

    pub fn overlays_in(&self, start: usize, end: usize) -> Vec<Value> {
        self.overlays_in_region(start, end, end)
    }

    pub fn overlays_in_region(
        &self,
        start: usize,
        end: usize,
        accessible_end: usize,
    ) -> Vec<Value> {
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

    pub fn highest_priority_overlay_at(&self, pos: usize, property: Value) -> Option<Value> {
        self.best_overlay_for(property, |overlay| overlay_covers_pos(overlay, pos))
    }

    pub fn highest_priority_overlay_for_inserted_char(
        &self,
        pos: usize,
        property: &Value,
    ) -> Option<Value> {
        self.best_overlay_for(*property, |overlay| {
            let Some(data) = overlay.as_overlay_data() else {
                return false;
            };
            if data.buffer.is_none() {
                return false;
            }
            !(data.start == pos && data.front_advance)
                && !(data.end == pos && !data.rear_advance)
                && data.start <= pos
                && pos <= data.end
        })
    }

    pub fn sort_overlay_ids_by_priority_desc(&self, overlay_ids: &mut [Value]) {
        overlay_ids.sort_by(|left, right| compare_overlay_precedence(*right, *left));
    }

    pub fn adjust_for_insert(&mut self, pos: usize, len: usize, before_markers: bool) {
        if len == 0 {
            return;
        }
        let live: Vec<Value> = self.overlays.iter().copied().collect();
        for overlay in &live {
            let _ = overlay.with_overlay_data_mut(|object| {
                let start = object.start;
                let end = object.end;
                let empty = start == end;

                if before_markers {
                    if start >= pos {
                        object.start += len;
                    }
                    if end >= pos {
                        object.end += len;
                    }
                    return;
                }

                if start > pos
                    || (start == pos && object.front_advance && (!empty || object.rear_advance))
                {
                    object.start += len;
                }

                if end > pos || (end == pos && object.rear_advance) {
                    object.end += len;
                }
            });
        }
        self.rebuild_indexes();
    }

    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;
        let live: Vec<Value> = self.overlays.iter().copied().collect();
        let mut evaporated = Vec::new();
        for overlay in &live {
            let should_evaporate = overlay
                .with_overlay_data_mut(|object| {
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
                        && plist::plist_get(object.plist, &Value::symbol("evaporate"))
                            .is_some_and(|v| v.is_truthy())
                    {
                        object.buffer = None;
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if should_evaporate {
                evaporated.push(*overlay);
            }
        }

        for overlay in evaporated {
            self.overlays.remove(&overlay);
        }
        self.rebuild_indexes();
    }

    pub fn set_front_advance(&mut self, overlay: Value, advance: bool) {
        let _ = overlay.with_overlay_data_mut(|data| {
            data.front_advance = advance;
        });
    }

    pub fn set_rear_advance(&mut self, overlay: Value, advance: bool) {
        let _ = overlay.with_overlay_data_mut(|data| {
            data.rear_advance = advance;
        });
    }

    pub fn get(&self, overlay: Value) -> Option<Overlay> {
        self.overlays
            .contains(&overlay)
            .then(|| overlay.as_overlay_data().unwrap().clone())
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

    pub(crate) fn dump_overlays(&self) -> Vec<Value> {
        self.overlays.iter().copied().collect()
    }

    pub(crate) fn from_dump(overlays: Vec<Value>) -> Self {
        let mut list = Self::new();
        for overlay in overlays {
            if overlay_live_buffer(overlay).is_some() {
                list.insert_overlay(overlay);
            }
        }
        list
    }

    fn best_overlay_for<F>(&self, property: Value, predicate: F) -> Option<Value>
    where
        F: Fn(Value) -> bool,
    {
        let mut best: Option<Value> = None;
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
        index: &mut BTreeMap<usize, BTreeSet<Value>>,
        boundary: usize,
        overlay: Value,
    ) {
        index.entry(boundary).or_default().insert(overlay);
    }

    fn remove_index_entry(
        index: &mut BTreeMap<usize, BTreeSet<Value>>,
        boundary: usize,
        overlay: Value,
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
        let live: Vec<Value> = self.overlays.iter().copied().collect();
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

fn overlay_live_buffer(overlay: Value) -> Option<crate::buffer::BufferId> {
    overlay.as_overlay_data().and_then(|d| d.buffer)
}

fn overlay_range(overlay: Value) -> Option<(usize, usize)> {
    let data = overlay.as_overlay_data()?;
    data.buffer.map(|_| (data.start, data.end))
}

fn overlay_covers_pos(overlay: Value, pos: usize) -> bool {
    let Some(data) = overlay.as_overlay_data() else {
        return false;
    };
    if data.buffer.is_none() {
        return false;
    }
    data.start <= pos && pos < data.end
}

fn overlay_overlaps_region(
    overlay: Value,
    start: usize,
    end: usize,
    accessible_end: usize,
) -> bool {
    let Some(data) = overlay.as_overlay_data() else {
        return false;
    };
    if data.buffer.is_none() {
        return false;
    }
    if data.start == data.end {
        return data.start == start
            || (start < data.start && data.start < end)
            || (data.start == end && end == accessible_end);
    }
    if start == end {
        return data.start <= start && start < data.end;
    }
    data.start < end && data.end > start
}

fn overlay_property_named(overlay: Value, prop_name: Value) -> Option<Value> {
    let plist = overlay.as_overlay_data()?.plist;
    plist::plist_get(plist, &prop_name)
}

fn compare_overlay_precedence(left: Value, right: Value) -> Ordering {
    let left_data = left.as_overlay_data();
    let right_data = right.as_overlay_data();
    let Some(left_overlay) = left_data.filter(|d| d.buffer.is_some()) else {
        return Ordering::Less;
    };
    let Some(right_overlay) = right_data.filter(|d| d.buffer.is_some()) else {
        return Ordering::Greater;
    };
    let (left_priority, left_subpriority) = overlay_priority(left_overlay);
    let (right_priority, right_subpriority) = overlay_priority(right_overlay);

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
        Some(value) => match value.kind() {
            ValueKind::Fixnum(n) => (n, 0),
            ValueKind::Cons => (
                priority_component(value.cons_car()),
                priority_component(value.cons_cdr()),
            ),
            _ => (0, 0),
        },
    }
}

fn priority_component(value: Value) -> i64 {
    match value.kind() {
        ValueKind::Fixnum(n) => n,
        _ => 0,
    }
}


fn plist_get_named(plist: Value, prop_name: &str) -> Option<Value> {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return None;
        };
        let pair_car = tail.cons_car();
        let pair_cdr = tail.cons_cdr();
        if !pair_cdr.is_cons() {
            return None;
        };
        if pair_car.as_symbol_name() == Some(prop_name) {
            return Some(pair_cdr.cons_car());
        }
        tail = pair_cdr.cons_cdr();
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
            roots.push(*overlay);
        }
    }
}

#[cfg(test)]
#[path = "overlay_test.rs"]
mod tests;
