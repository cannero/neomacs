//! GNU-style text interval storage for buffers and strings.
//!
//! GNU Emacs represents text properties as an interval tree rooted from the
//! owning string or buffer.  Neomacs keeps the existing Rust API name
//! (`TextPropertyTable`) for callers, and follows GNU's mutation shape: split
//! at the edit range, change the affected interval plists, then merge adjacent
//! intervals with equal plists.

use std::collections::HashMap;

use crate::emacs_core::value::{Value, eq_value};
use crate::gc_trace::GcTrace;

// ---------------------------------------------------------------------------
// PropertyInterval
// ---------------------------------------------------------------------------

/// Public snapshot of one text-property interval.
///
/// Runtime storage uses the same start/end/plist shape.  This type remains the
/// serialization and inspection shape used by pdump/tests.  Bounds are character
/// positions, matching GNU intervals; buffer owners convert byte positions at
/// the boundary.
#[derive(Clone, Debug)]
pub struct PropertyInterval {
    /// Character position where this interval starts (inclusive).
    pub start: usize,
    /// Character position where this interval ends (exclusive).
    pub end: usize,
    /// Snapshot map for the interval plist.
    pub properties: HashMap<Value, Value>,
    /// Property keys in GNU plist order, newest first.
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

    fn from_plist(start: usize, end: usize, plist: &[(Value, Value)]) -> Self {
        let mut properties = HashMap::new();
        for (key, value) in plist.iter().rev() {
            properties.insert(*key, *value);
        }
        let mut key_order = Vec::new();
        for (key, _) in plist {
            if !key_order.iter().any(|seen| eq_value(seen, key)) {
                key_order.push(*key);
            }
        }
        Self {
            start,
            end,
            properties,
            key_order,
        }
    }

    fn into_plist(self) -> Vec<(Value, Value)> {
        let mut plist = Vec::new();
        for key in &self.key_order {
            if let Some(value) = self.properties.get(key)
                && !plist.iter().any(|(seen, _)| eq_value(seen, key))
            {
                plist.push((*key, *value));
            }
        }
        for (key, value) in self.properties {
            if !plist.iter().any(|(seen, _)| eq_value(seen, &key)) {
                plist.push((key, value));
            }
        }
        plist
    }

    /// Iterate properties in GNU plist order.
    pub fn ordered_properties(&self) -> impl Iterator<Item = (Value, &Value)> {
        self.key_order
            .iter()
            .filter_map(move |key| self.properties.get(key).map(|value| (*key, value)))
    }
}

type IntervalPlist = Vec<(Value, Value)>;

#[derive(Clone, Debug)]
struct IntervalRun {
    start: usize,
    end: usize,
    plist: IntervalPlist,
}

impl IntervalRun {
    fn new(start: usize, end: usize, plist: IntervalPlist) -> Self {
        Self { start, end, plist }
    }

    fn default(start: usize, end: usize) -> Self {
        Self::new(start, end, Vec::new())
    }

    fn is_empty_plist(&self) -> bool {
        self.plist.is_empty()
    }
}

fn plist_get(plist: &[(Value, Value)], key: Value) -> Option<&Value> {
    plist
        .iter()
        .find_map(|(name, value)| eq_value(name, &key).then_some(value))
}

fn plist_put_replace(plist: &mut IntervalPlist, key: Value, value: Value) -> bool {
    for (name, existing) in plist.iter_mut() {
        if eq_value(name, &key) {
            if eq_value(existing, &value) {
                return false;
            }
            *existing = value;
            return true;
        }
    }
    plist.insert(0, (key, value));
    true
}

fn plist_remove(plist: &mut IntervalPlist, key: Value) -> bool {
    let before = plist.len();
    plist.retain(|(name, _)| !eq_value(name, &key));
    before != plist.len()
}

fn plists_equal_eq(left: &[(Value, Value)], right: &[(Value, Value)]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().all(|(left_key, left_value)| {
        right.iter().any(|(right_key, right_value)| {
            eq_value(left_key, right_key) && eq_value(left_value, right_value)
        })
    })
}

// ---------------------------------------------------------------------------
// TextPropertyTable
// ---------------------------------------------------------------------------

/// GNU-style text-property intervals.
#[derive(Clone)]
pub struct TextPropertyTable {
    runs: Vec<IntervalRun>,
}

impl TextPropertyTable {
    pub fn new() -> Self {
        Self { runs: Vec::new() }
    }

    pub fn put_property(&mut self, start: usize, end: usize, name: Value, value: Value) -> bool {
        if start >= end {
            return false;
        }

        let mut runs = self.runs.clone();
        runs = split_runs_at(runs, &[start, end]);
        runs = cover_range_with_default_intervals(runs, start, end);

        let mut changed = false;
        for run in &mut runs {
            if run.start < end && run.end > start && plist_put_replace(&mut run.plist, name, value)
            {
                changed = true;
            }
        }

        self.replace_runs(runs);
        changed
    }

    pub(crate) fn from_plist_runs(runs: Vec<(usize, usize, Vec<(Value, Value)>)>) -> Self {
        let mut table = Self::new();
        table.replace_runs(
            runs.into_iter()
                .map(|(start, end, plist)| IntervalRun::new(start, end, plist))
                .collect(),
        );
        table
    }

    pub fn get_property(&self, pos: usize, name: Value) -> Option<&Value> {
        let idx = self.interval_containing_index(pos)?;
        plist_get(&self.runs[idx].plist, name)
    }

    pub fn get_properties(&self, pos: usize) -> HashMap<Value, Value> {
        let Some(idx) = self.interval_containing_index(pos) else {
            return HashMap::new();
        };
        PropertyInterval::from_plist(pos, pos + 1, &self.runs[idx].plist).properties
    }

    pub fn get_properties_ordered(&self, pos: usize) -> Vec<(Value, Value)> {
        let Some(idx) = self.interval_containing_index(pos) else {
            return Vec::new();
        };
        self.runs[idx].plist.clone()
    }

    pub fn remove_property(&mut self, start: usize, end: usize, name: Value) -> bool {
        if start >= end {
            return false;
        }

        let mut runs = split_runs_at(self.runs.clone(), &[start, end]);
        let mut changed = false;
        for run in &mut runs {
            if run.start < end && run.end > start && plist_remove(&mut run.plist, name) {
                changed = true;
            }
        }
        self.replace_runs(runs);
        changed
    }

    pub fn remove_all_properties(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        let mut runs = split_runs_at(self.runs.clone(), &[start, end]);
        for run in &mut runs {
            if run.start < end && run.end > start {
                run.plist.clear();
            }
        }
        self.replace_runs(runs);
    }

    pub fn next_property_change(&self, pos: usize) -> Option<usize> {
        let runs = &self.runs;
        if runs.is_empty() {
            return None;
        }

        for (idx, run) in runs.iter().enumerate() {
            if pos < run.start {
                return next_non_default_start(&runs, idx);
            }

            if run.start <= pos && pos < run.end {
                if run.is_empty_plist() {
                    return next_non_default_start(&runs, idx + 1);
                }
                return Some(run.end);
            }
        }

        None
    }

    pub fn previous_property_change(&self, pos: usize) -> Option<usize> {
        if pos == 0 {
            return None;
        }
        let runs = &self.runs;
        if runs.is_empty() {
            return None;
        }

        let scan_pos = pos - 1;
        for idx in (0..runs.len()).rev() {
            let run = &runs[idx];
            if scan_pos >= run.end {
                if !run.is_empty_plist() {
                    return Some(run.end);
                }
                continue;
            }

            if run.start <= scan_pos && scan_pos < run.end {
                if run.is_empty_plist() {
                    return previous_non_default_end(&runs, idx);
                }
                return Some(run.start);
            }
        }

        None
    }

    pub fn adjust_for_insert(&mut self, pos: usize, len: usize) {
        if len == 0 {
            return;
        }

        let mut shifted = Vec::new();
        for run in self.runs.clone() {
            if run.end <= pos {
                shifted.push(run);
            } else if run.start >= pos {
                shifted.push(IntervalRun::new(run.start + len, run.end + len, run.plist));
            } else {
                shifted.push(IntervalRun::new(run.start, pos, run.plist.clone()));
                shifted.push(IntervalRun::default(pos, pos + len));
                shifted.push(IntervalRun::new(pos + len, run.end + len, run.plist));
            }
        }
        self.replace_runs(shifted);
    }

    pub fn adjust_for_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        let len = end - start;
        let mut shifted = Vec::new();
        for mut run in self.runs.clone() {
            if run.end <= start {
                shifted.push(run);
            } else if run.start >= end {
                run.start -= len;
                run.end -= len;
                shifted.push(run);
            } else if run.start < start && run.end > end {
                run.end -= len;
                shifted.push(run);
            } else if run.start < start {
                run.end = start;
                shifted.push(run);
            } else if run.end > end {
                run.start = start;
                run.end -= len;
                shifted.push(run);
            }
        }
        self.replace_runs(shifted);
    }

    pub fn intervals_snapshot(&self) -> Vec<PropertyInterval> {
        self.runs
            .iter()
            .filter(|run| !run.plist.is_empty())
            .map(|run| PropertyInterval::from_plist(run.start, run.end, &run.plist))
            .collect()
    }

    pub(crate) fn try_for_each_interval_in_range<E>(
        &self,
        start: usize,
        end: usize,
        mut f: impl FnMut(usize, usize, &[(Value, Value)]) -> Result<(), E>,
    ) -> Result<(), E> {
        if start >= end {
            return Ok(());
        }
        for run in &self.runs {
            if run.end <= start {
                continue;
            }
            if run.start >= end {
                break;
            }
            f(run.start, run.end, &run.plist)?;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn slice(&self, start: usize, end: usize) -> TextPropertyTable {
        if start >= end {
            return TextPropertyTable::new();
        }

        let intervals = self
            .intervals_snapshot()
            .into_iter()
            .filter_map(|interval| {
                if interval.end <= start || interval.start >= end {
                    return None;
                }
                let new_start = interval.start.max(start) - start;
                let new_end = interval.end.min(end) - start;
                (new_start < new_end).then_some(PropertyInterval {
                    start: new_start,
                    end: new_end,
                    properties: interval.properties,
                    key_order: interval.key_order,
                })
            })
            .collect();
        TextPropertyTable::from_dump(intervals)
    }

    pub fn append_shifted(&mut self, other: &TextPropertyTable, offset: usize) {
        let mut runs = self.runs.clone();
        runs.extend(other.intervals_snapshot().into_iter().map(|interval| {
            IntervalRun::new(
                interval.start + offset,
                interval.end + offset,
                interval.into_plist(),
            )
        }));
        self.replace_runs(runs);
    }

    pub fn merge_missing_shifted(&mut self, other: &TextPropertyTable, offset: usize) {
        let source_runs: Vec<IntervalRun> = other
            .intervals_snapshot()
            .into_iter()
            .map(|interval| {
                IntervalRun::new(
                    interval.start + offset,
                    interval.end + offset,
                    interval.into_plist(),
                )
            })
            .collect();
        if source_runs.is_empty() {
            return;
        }

        let mut runs = self.runs.clone();
        for source in source_runs {
            if source.is_empty_plist() {
                continue;
            }
            runs = split_runs_at(runs, &[source.start, source.end]);
            runs = cover_range_with_default_intervals(runs, source.start, source.end);
            for run in &mut runs {
                if run.start >= source.end || run.end <= source.start {
                    continue;
                }
                for (name, value) in &source.plist {
                    if plist_get(&run.plist, *name).is_none() {
                        run.plist.insert(0, (*name, *value));
                    }
                }
            }
        }
        self.replace_runs(runs);
    }

    pub(crate) fn dump_intervals(&self) -> Vec<PropertyInterval> {
        self.intervals_snapshot()
    }

    pub(crate) fn from_dump(intervals: Vec<PropertyInterval>) -> Self {
        let mut table = Self::new();
        table.replace_runs(
            intervals
                .into_iter()
                .map(|interval| {
                    IntervalRun::new(interval.start, interval.end, interval.into_plist())
                })
                .collect(),
        );
        table
    }

    pub(crate) fn for_each_root(&self, mut f: impl FnMut(Value)) {
        for run in &self.runs {
            for (key, value) in &run.plist {
                f(*key);
                f(*value);
            }
        }
    }

    fn replace_runs(&mut self, runs: Vec<IntervalRun>) {
        self.runs = normalize_runs(runs);
    }

    fn interval_containing_index(&self, pos: usize) -> Option<usize> {
        let idx = self.runs.partition_point(|run| run.end <= pos);
        if idx < self.runs.len() {
            let run = &self.runs[idx];
            if run.start <= pos && pos < run.end {
                return Some(idx);
            }
        }
        None
    }
}

fn split_runs_at(mut runs: Vec<IntervalRun>, boundaries: &[usize]) -> Vec<IntervalRun> {
    let mut bounds: Vec<usize> = boundaries.to_vec();
    bounds.sort_unstable();
    bounds.dedup();
    let mut result = Vec::with_capacity(runs.len() + bounds.len());

    for run in runs.drain(..) {
        let mut start = run.start;
        for boundary in bounds.iter().copied() {
            if start < boundary && boundary < run.end {
                result.push(IntervalRun::new(start, boundary, run.plist.clone()));
                start = boundary;
            }
        }
        result.push(IntervalRun::new(start, run.end, run.plist));
    }

    result
}

fn cover_range_with_default_intervals(
    mut runs: Vec<IntervalRun>,
    start: usize,
    end: usize,
) -> Vec<IntervalRun> {
    runs.sort_by_key(|run| run.start);
    let mut result = Vec::new();
    let mut cursor = start;
    let mut covered = false;

    for run in runs {
        if run.end <= start {
            result.push(run);
            continue;
        }

        if run.start >= end {
            if !covered && cursor < end {
                result.push(IntervalRun::default(cursor, end));
                covered = true;
            }
            result.push(run);
            continue;
        }

        if cursor < run.start {
            result.push(IntervalRun::default(cursor, run.start));
        }
        cursor = cursor.max(run.end);
        result.push(run);
    }

    if !covered && cursor < end {
        result.push(IntervalRun::default(cursor, end));
    }

    result
}

fn normalize_runs(mut runs: Vec<IntervalRun>) -> Vec<IntervalRun> {
    runs.retain(|run| run.start < run.end);
    runs.sort_by_key(|run| run.start);

    let mut normalized: Vec<IntervalRun> = Vec::new();
    for mut run in runs {
        if let Some(last) = normalized.last_mut() {
            if run.start < last.end {
                run.start = last.end;
                if run.start >= run.end {
                    continue;
                }
            }
            if last.end == run.start && plists_equal_eq(&last.plist, &run.plist) {
                last.end = run.end;
                continue;
            }
        }
        normalized.push(run);
    }

    while normalized.first().is_some_and(|run| run.plist.is_empty()) {
        normalized.remove(0);
    }
    while normalized.last().is_some_and(|run| run.plist.is_empty()) {
        normalized.pop();
    }

    if normalized.iter().all(|run| run.plist.is_empty()) {
        Vec::new()
    } else {
        normalized
    }
}

fn next_non_default_start(runs: &[IntervalRun], start_idx: usize) -> Option<usize> {
    runs.iter()
        .skip(start_idx)
        .find(|run| !run.is_empty_plist())
        .map(|run| run.start)
}

fn previous_non_default_end(runs: &[IntervalRun], before_idx: usize) -> Option<usize> {
    runs.iter()
        .take(before_idx)
        .rev()
        .find(|run| !run.is_empty_plist())
        .map(|run| run.end)
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
