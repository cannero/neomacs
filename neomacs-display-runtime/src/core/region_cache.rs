//! Pure Rust region cache for caching position-to-value mappings.
//!
//! This is a reimplementation of Emacs's `region-cache.c`. It maintains a
//! sorted array of boundaries with a gap (like a gap buffer) that maps buffer
//! positions to integer values. Typical use: caching "known" vs "unknown"
//! regions so that expensive scans (e.g., searching for newlines) can skip
//! over regions whose properties have already been determined.
//!
//! # Design
//!
//! The cache stores a sequence of `Boundary` entries, each carrying a position
//! and a value.  A boundary's value applies to all positions from that
//! boundary up to (but not including) the next boundary.  The first boundary
//! always sits at position `buffer_beg` so every position has a defined value.
//!
//! To make insertions and deletions cheap near the editing point, the array
//! uses a gap -- exactly the same idea as an Emacs gap buffer, but operating
//! on `Boundary` elements instead of bytes.
//!
//! Boundaries before the gap store positions relative to `buffer_beg`;
//! boundaries after the gap store positions relative to `buffer_end`.
//! When the buffer changes size, `buffer_end` floats and all end-relative
//! boundaries automatically adjust.

use std::fmt;

/// Default number of empty slots to add when growing the boundary array.
const NEW_CACHE_GAP: usize = 40;

/// If an invalidation would discard information about more than this many
/// positions, trigger a revalidation first to preserve it.
const PRESERVE_THRESHOLD: usize = 500;

/// A single boundary in the region cache.
///
/// The boundary's `value` applies to all positions at and after this boundary,
/// up to (but not including) the next boundary's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boundary {
    /// Buffer position stored as a signed offset relative to either
    /// `buffer_beg` (if before the gap) or `buffer_end` (if after the gap).
    pos: isize,
    /// Cached value for positions at and after this boundary.
    pub value: i32,
}

/// A gap-based sorted array of boundaries that caches position-to-value
/// mappings over buffer regions.
///
/// The cache always contains at least one boundary at position `buffer_beg`
/// (the beginning of the buffer).  Values are looked up via binary search
/// in O(log n) time.
pub struct RegionCache {
    /// The boundary array, including the gap.
    boundaries: Vec<Boundary>,

    /// Index where the gap starts (logical index).
    gap_start: usize,

    /// Length of the gap (number of unused slots).
    gap_len: usize,

    /// Number of live boundaries (excluding gap slots).
    cache_len: usize,

    /// Unchanged prefix length: positions `[buffer_beg, buffer_beg + beg_unchanged)`
    /// are still valid since the last revalidation.
    beg_unchanged: usize,

    /// Unchanged suffix length: positions `(buffer_end - end_unchanged, buffer_end]`
    /// are still valid since the last revalidation.
    end_unchanged: usize,

    /// The cache's current basis: first position in the buffer.
    /// This is the value of buffer_beg at the time of last revalidation.
    buffer_beg: usize,

    /// The cache's current basis: one-past-the-last position.
    /// This may be stale if modifications have occurred since the last
    /// revalidation.  Boundaries after the gap are relative to this value.
    buffer_end: usize,

    /// The actual current buffer end.  Updated eagerly by `adjust_for_insert`
    /// and `adjust_for_delete`.  `revalidate` uses this to update
    /// `buffer_end` to the correct current value.
    actual_buffer_end: usize,

    /// The default value assigned to unknown regions.
    default_value: i32,
}

impl RegionCache {
    // ===== Construction =====

    /// Create a new, empty region cache.
    ///
    /// Every position initially maps to `default_value`.  The cache is set up
    /// for a buffer beginning at position 0 with length 0.
    pub fn new(default_value: i32) -> Self {
        let gap_len = NEW_CACHE_GAP;
        let total = 1 + gap_len;
        let mut boundaries = Vec::with_capacity(total);
        // The sentinel boundary at the buffer beginning.
        boundaries.push(Boundary {
            pos: 0,
            value: default_value,
        });
        // Fill the rest with gap slots.
        boundaries.resize(
            total,
            Boundary {
                pos: 0,
                value: default_value,
            },
        );

        RegionCache {
            boundaries,
            gap_start: 1,
            gap_len,
            cache_len: 1,
            beg_unchanged: 0,
            end_unchanged: 0,
            buffer_beg: 0,
            buffer_end: 0,
            actual_buffer_end: 0,
            default_value,
        }
    }

    /// Set the buffer extent.  Must be called before any queries if the buffer
    /// is non-empty.
    pub fn set_buffer_extent(&mut self, beg: usize, end: usize) {
        debug_assert!(beg <= end);
        self.buffer_beg = beg;
        self.buffer_end = end;
        self.actual_buffer_end = end;
        let extent = end - beg;
        self.beg_unchanged = extent;
        self.end_unchanged = extent;
    }

    /// Return the number of live boundaries in the cache.
    pub fn len(&self) -> usize {
        self.cache_len
    }

    /// Return true if the cache contains only the sentinel boundary.
    pub fn is_empty(&self) -> bool {
        self.cache_len <= 1
    }

    /// Return the default value used for unknown regions.
    pub fn default_value(&self) -> i32 {
        self.default_value
    }

    // ===== Position resolution =====

    /// Return the absolute position of the `i`-th logical boundary.
    #[inline]
    fn boundary_pos(&self, i: usize) -> usize {
        if i < self.gap_start {
            // Before the gap -- position is relative to buffer_beg.
            (self.buffer_beg as isize + self.boundaries[i].pos) as usize
        } else {
            // After the gap -- stored at physical index `i + gap_len`,
            // position is relative to buffer_end.
            let phys = i + self.gap_len;
            (self.buffer_end as isize + self.boundaries[phys].pos) as usize
        }
    }

    /// Return the value of the `i`-th logical boundary.
    #[inline]
    fn boundary_value(&self, i: usize) -> i32 {
        if i < self.gap_start {
            self.boundaries[i].value
        } else {
            self.boundaries[i + self.gap_len].value
        }
    }

    /// Set the value of the `i`-th logical boundary.
    #[inline]
    fn set_boundary_value(&mut self, i: usize, value: i32) {
        if i < self.gap_start {
            self.boundaries[i].value = value;
        } else {
            self.boundaries[i + self.gap_len].value = value;
        }
    }

    // ===== Binary search =====

    /// Return the index of the last boundary at or before `pos`.
    ///
    /// This is O(log n) in the number of boundaries.
    fn find_boundary(&self, pos: usize) -> usize {
        let mut low: usize = 0;
        let mut high: usize = self.cache_len;

        while low + 1 < high {
            let mid = low + (high - low) / 2;
            let boundary = self.boundary_pos(mid);
            if pos < boundary {
                high = mid;
            } else {
                low = mid;
            }
        }

        debug_assert!(self.boundary_pos(low) <= pos);
        debug_assert!(low + 1 >= self.cache_len || self.boundary_pos(low + 1) > pos);

        low
    }

    // ===== Gap movement and resizing =====

    /// Move the gap so it starts at logical index `pos`, ensuring at least
    /// `min_size` free slots.
    fn move_gap(&mut self, pos: usize, min_size: usize) {
        // The sentinel at index 0 must always be before the gap.
        debug_assert!(pos > 0 && pos <= self.cache_len);

        let buffer_beg = self.buffer_beg as isize;
        let buffer_end = self.buffer_end as isize;

        // Move gap right: copy boundaries from after-gap to before-gap,
        // converting end-relative positions to beg-relative.
        while self.gap_start < pos {
            let phys_src = self.gap_start + self.gap_len;
            let beg_relative_pos = buffer_end + self.boundaries[phys_src].pos - buffer_beg;
            self.boundaries[self.gap_start] = Boundary {
                pos: beg_relative_pos,
                value: self.boundaries[phys_src].value,
            };
            self.gap_start += 1;
        }

        // Grow the gap if needed.
        if self.gap_len < min_size {
            let extra = min_size - self.gap_len + NEW_CACHE_GAP;
            let old_total = self.boundaries.len();
            self.boundaries.resize(
                old_total + extra,
                Boundary {
                    pos: 0,
                    value: self.default_value,
                },
            );

            // Shift after-gap boundaries to the new end.
            let after_count = self.cache_len - self.gap_start;
            for k in (0..after_count).rev() {
                let old_phys = self.gap_start + self.gap_len + k;
                let new_phys = self.gap_start + self.gap_len + extra + k;
                self.boundaries[new_phys] = self.boundaries[old_phys];
            }

            self.gap_len += extra;
        }

        // Move gap left: copy boundaries from before-gap to after-gap,
        // converting beg-relative positions to end-relative.
        while pos < self.gap_start {
            self.gap_start -= 1;
            let phys_dst = self.gap_start + self.gap_len;
            let end_relative_pos = self.boundaries[self.gap_start].pos + buffer_beg - buffer_end;
            self.boundaries[phys_dst] = Boundary {
                pos: end_relative_pos,
                value: self.boundaries[self.gap_start].value,
            };
        }
    }

    // ===== Insertion and deletion of boundaries =====

    /// Insert a new boundary at logical index `i` with the given absolute
    /// `pos` and `value`.
    fn insert_boundary(&mut self, i: usize, pos: usize, value: i32) {
        debug_assert!(i > 0 && i <= self.cache_len);
        debug_assert!(self.boundary_pos(i - 1) < pos);
        debug_assert!(i == self.cache_len || pos < self.boundary_pos(i));
        debug_assert!(self.boundary_value(i - 1) != value);

        self.move_gap(i, 1);

        self.boundaries[i] = Boundary {
            pos: pos as isize - self.buffer_beg as isize,
            value,
        };
        self.gap_start += 1;
        self.gap_len -= 1;
        self.cache_len += 1;
    }

    /// Delete boundaries with logical indices in `[start, end)`.
    fn delete_boundaries(&mut self, start: usize, end: usize) {
        let count = end - start;
        debug_assert!(start <= end && end <= self.cache_len);
        // Never delete the sentinel.
        debug_assert!(!(start == 0 && end >= 1));

        if count == 0 {
            return;
        }

        if self.gap_start <= start {
            self.move_gap(start, 0);
            self.gap_len += count;
        } else if end <= self.gap_start {
            self.move_gap(end, 0);
            self.gap_start -= count;
            self.gap_len += count;
        } else {
            // Gap is inside the deleted range -- just expand it.
            self.gap_start = start;
            self.gap_len += count;
        }

        self.cache_len -= count;
    }

    // ===== Core: set_cache_region =====

    /// Set the value for the region `[start, end)` to `value`.
    ///
    /// This is the internal workhorse.  It removes any existing boundaries
    /// strictly inside the region and creates / removes boundaries at the
    /// edges so that the region carries the requested value, merging with
    /// adjacent regions where possible.
    fn set_cache_region(&mut self, start: usize, end: usize, value: i32) {
        debug_assert!(start <= end);
        if start == end {
            return;
        }
        debug_assert!(self.buffer_beg <= start && end <= self.buffer_end);

        let start_ix = self.find_boundary(start);
        let end_ix = self.find_boundary(end - 1) + 1;

        // Remember the value that was in effect just before `end`.
        let value_at_end = self.boundary_value(end_ix - 1);

        // Delete all boundaries strictly inside the region.
        self.delete_boundaries(start_ix + 1, end_ix);

        // --- Left edge ---
        if self.boundary_pos(start_ix) == start {
            if start_ix > 0 && self.boundary_value(start_ix - 1) == value {
                // Merge with predecessor.
                self.delete_boundaries(start_ix, start_ix + 1);
            } else {
                self.set_boundary_value(start_ix, value);
            }
        } else {
            // The boundary governing `start` begins before `start`.
            if self.boundary_value(start_ix) != value {
                self.insert_boundary(start_ix + 1, start, value);
            }
        }

        // Recalculate: find the boundary that now governs `start`.
        let start_ix = self.find_boundary(start);
        let end_ix = start_ix + 1;

        // --- Right edge ---
        if end == self.buffer_end {
            // Nothing after the region -- no boundary needed.
        } else if end_ix >= self.cache_len || end < self.boundary_pos(end_ix) {
            // No boundary at `end`, but we may need one.
            if value_at_end != value {
                self.insert_boundary(end_ix, end, value_at_end);
            }
        } else {
            // There is a boundary at `end`; remove it if redundant.
            if value == self.boundary_value(end_ix) {
                self.delete_boundaries(end_ix, end_ix + 1);
            }
        }
    }

    // ===== Revalidation =====

    /// Clean the cache after buffer modifications.
    ///
    /// Buffer edits are recorded via `notify_change` as unchanged-prefix /
    /// unchanged-suffix lengths.  This function discards stale boundaries in
    /// the modified region, adjusts the basis (`buffer_beg` / `buffer_end`),
    /// and collapses redundant boundaries.
    ///
    /// Mirrors `revalidate_region_cache` from the C implementation.
    fn revalidate(&mut self) {
        // If everything is still valid, nothing to do.
        // (Use `>` not `>=`: consider insertions where beg_unchanged +
        // end_unchanged == old extent but the buffer grew.)
        if self.buffer_beg + self.beg_unchanged > self.buffer_end - self.end_unchanged {
            // Still need to sync the actual buffer end.
            self.buffer_end = self.actual_buffer_end;
            return;
        }

        let new_buffer_end = self.actual_buffer_end;

        if self.buffer_beg + self.beg_unchanged == self.buffer_end - self.end_unchanged {
            // The old modified region was empty: in the old buffer there was
            // no gap between unchanged head and unchanged tail.  This happens
            // for pure insertions.  Move the boundary gap to the split point,
            // then update the basis so the new (non-empty) modified region
            // becomes visible.
            let split_pos = self.buffer_beg + self.beg_unchanged;
            let split_ix = self.find_boundary(split_pos) + 1;
            self.move_gap(split_ix, 0);

            // Update basis to reflect the new buffer size.
            self.buffer_end = new_buffer_end;

            // Now the modified region may be non-empty in the new buffer.
            let mod_start = self.buffer_beg + self.beg_unchanged;
            let mod_end = self.buffer_end - self.end_unchanged;
            if mod_start < mod_end {
                self.set_cache_region(mod_start, mod_end, self.default_value);
            }
        } else {
            // The old modified region was non-empty.  Invalidate it under the
            // old basis, then move the gap and update the basis.

            let mod_start = self.buffer_beg + self.beg_unchanged;
            let mod_end_old = self.buffer_end - self.end_unchanged;

            if mod_start < mod_end_old {
                self.set_cache_region(mod_start, mod_end_old, self.default_value);
            }

            let modified_ix = self.find_boundary(mod_start) + 1;
            self.move_gap(modified_ix, 0);

            // Update basis.
            self.buffer_end = new_buffer_end;

            // After changing the basis, boundaries that previously bracketed
            // the modified region may now coincide.  Collapse them.
            if modified_ix < self.cache_len
                && self.boundary_pos(modified_ix - 1) == self.boundary_pos(modified_ix)
            {
                let value_after = self.boundary_value(modified_ix);
                if modified_ix >= 2 && value_after == self.boundary_value(modified_ix - 2) {
                    self.delete_boundaries(modified_ix - 1, modified_ix + 1);
                } else {
                    self.set_boundary_value(modified_ix - 1, value_after);
                    self.delete_boundaries(modified_ix, modified_ix + 1);
                }
            }
        }

        // Mark the entire cache valid.
        let extent = self.buffer_end - self.buffer_beg;
        self.beg_unchanged = extent;
        self.end_unchanged = extent;
    }

    // ===== Public interface =====

    /// Mark the region `[start, end)` as having the given `value`.
    ///
    /// Adjacent regions with the same value are automatically merged.
    pub fn set_region(&mut self, start: usize, end: usize, value: i32) {
        debug_assert!(start <= end);
        if start == end {
            return;
        }
        self.revalidate();
        self.set_cache_region(start, end, value);
    }

    /// Mark the region `[start, end)` as "known" (value = 1).
    ///
    /// This is the equivalent of `know_region_cache` in the C implementation.
    pub fn know_region(&mut self, start: usize, end: usize) {
        self.set_region(start, end, 1);
    }

    /// Look up the value at position `pos`.
    ///
    /// Returns the cached value.  If `pos` is beyond `buffer_end`, returns the
    /// default value.
    ///
    /// **Note**: This performs a read-only lookup and does NOT trigger
    /// revalidation.  If you have pending modifications, call `revalidate()`
    /// or use `forward()`/`backward()` which do revalidate.
    pub fn get_value(&self, pos: usize) -> i32 {
        if pos >= self.actual_buffer_end {
            return self.default_value;
        }
        let i = self.find_boundary(pos);
        self.boundary_value(i)
    }

    /// Look up the value at position `pos` after ensuring the cache is valid.
    pub fn get_value_revalidated(&mut self, pos: usize) -> i32 {
        self.revalidate();
        self.get_value(pos)
    }

    /// Scan forward from `pos`.
    ///
    /// Returns `(value, next)` where `value` is the cached value at `pos` and
    /// `next` is the nearest position after `pos` where the value changes.
    pub fn forward(&mut self, pos: usize) -> (i32, usize) {
        self.revalidate();

        if pos >= self.buffer_end {
            return (self.default_value, self.buffer_end);
        }

        let i = self.find_boundary(pos);
        let i_value = self.boundary_value(i);

        // Scan forward to find the next boundary with a different value.
        let mut j = i + 1;
        while j < self.cache_len {
            if self.boundary_value(j) != i_value {
                break;
            }
            j += 1;
        }

        let next = if j < self.cache_len {
            self.boundary_pos(j)
        } else {
            self.buffer_end
        };

        (i_value, next)
    }

    /// Scan backward from `pos`.
    ///
    /// Returns `(value, next)` where `value` is the cached value just before
    /// `pos` and `next` is the nearest position before `pos` where the value
    /// changes.
    pub fn backward(&mut self, pos: usize) -> (i32, usize) {
        self.revalidate();

        if pos <= self.buffer_beg {
            return (self.default_value, self.buffer_beg);
        }

        let i = self.find_boundary(pos - 1);
        let i_value = self.boundary_value(i);

        // Scan backward to find the previous boundary with a different value.
        let mut j = i as isize - 1;
        while j >= 0 {
            if self.boundary_value(j as usize) != i_value {
                break;
            }
            j -= 1;
        }

        let next = if j >= 0 {
            self.boundary_pos((j + 1) as usize)
        } else {
            self.buffer_beg
        };

        (i_value, next)
    }

    /// Reset a region `[start, end)` to the default value.
    pub fn invalidate_region(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.revalidate();
        self.set_cache_region(start, end, self.default_value);
    }

    /// Clear the entire cache, resetting every position to the default value.
    pub fn invalidate_all(&mut self) {
        let default = self.default_value;
        let beg = self.buffer_beg;
        let end = self.actual_buffer_end;
        *self = RegionCache::new(default);
        self.set_buffer_extent(beg, end);
    }

    /// Notify the cache that `head` positions at the start of the buffer and
    /// `tail` positions at the end are unchanged.
    ///
    /// This is the equivalent of `invalidate_region_cache` in the C code.
    /// The modified region is `[buffer_beg + head, old_buffer_end - tail)`.
    ///
    /// **Important**: `head` and `tail` are measured from the old buffer
    /// extent (before the modification).
    fn notify_change(&mut self, head: usize, tail: usize) {
        // If discarding too much information, revalidate first to preserve it.
        let beg_u = self.buffer_beg + self.beg_unchanged;
        let end_u = self.buffer_end.saturating_sub(self.end_unchanged);

        let new_mod_end = self.buffer_end.saturating_sub(tail);
        let new_mod_start = self.buffer_beg + head;

        if beg_u > new_mod_end && beg_u - new_mod_end > PRESERVE_THRESHOLD {
            self.revalidate();
        } else if new_mod_start > end_u && new_mod_start - end_u > PRESERVE_THRESHOLD {
            self.revalidate();
        }

        if head < self.beg_unchanged {
            self.beg_unchanged = head;
        }
        if tail < self.end_unchanged {
            self.end_unchanged = tail;
        }
    }

    /// Adjust the cache for an insertion of `length` positions at `pos`.
    ///
    /// This records that text was inserted but does not immediately clean up
    /// stale boundaries; that happens lazily on the next query or
    /// `set_region` call.
    pub fn adjust_for_insert(&mut self, pos: usize, length: usize) {
        if length == 0 {
            return;
        }

        let head = pos - self.buffer_beg;
        let tail = self.buffer_end - pos;

        // Update the actual buffer end eagerly.
        self.actual_buffer_end += length;

        // Record the modification (against the old basis).
        self.notify_change(head, tail);
    }

    /// Adjust the cache for a deletion of positions `[from, to)`.
    ///
    /// This records that text was deleted but does not immediately clean up
    /// stale boundaries; that happens lazily on the next query or
    /// `set_region` call.
    pub fn adjust_for_delete(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let length = to - from;

        let head = from - self.buffer_beg;
        let tail = self.buffer_end - to;

        // Update the actual buffer end eagerly.
        self.actual_buffer_end -= length;

        // Record the modification (against the old basis).
        self.notify_change(head, tail);
    }
}

impl fmt::Debug for RegionCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionCache {{ buffer: {}..{} (actual_end: {}), boundaries: {}, gap: {}+{} }}",
            self.buffer_beg,
            self.buffer_end,
            self.actual_buffer_end,
            self.cache_len,
            self.gap_start,
            self.gap_len
        )?;
        let beg_u = self.buffer_beg + self.beg_unchanged;
        let end_u = self.buffer_end.saturating_sub(self.end_unchanged);
        write!(f, " modified: {}..{}", beg_u, end_u)?;
        for i in 0..self.cache_len {
            let pos = self.boundary_pos(i);
            let val = self.boundary_value(i);
            let prefix = if pos < beg_u {
                'v'
            } else if pos == beg_u {
                '-'
            } else {
                ' '
            };
            let suffix = if pos > end_u {
                '^'
            } else if pos == end_u {
                '-'
            } else {
                ' '
            };
            write!(f, "\n  {}{}{}: {}", prefix, suffix, pos, val)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a cache for a buffer of `size` positions starting at 0.
    fn make_cache(size: usize) -> RegionCache {
        let mut c = RegionCache::new(0);
        c.set_buffer_extent(0, size);
        c
    }

    #[test]
    fn test_new_empty() {
        let c = RegionCache::new(0);
        assert_eq!(c.len(), 1); // sentinel only
        assert!(c.is_empty());
        assert_eq!(c.default_value(), 0);
    }

    #[test]
    fn test_new_with_nonzero_default() {
        let c = RegionCache::new(42);
        assert_eq!(c.default_value(), 42);
    }

    #[test]
    fn test_set_buffer_extent() {
        let mut c = RegionCache::new(0);
        c.set_buffer_extent(0, 100);
        assert_eq!(c.get_value(0), 0);
        assert_eq!(c.get_value(50), 0);
        assert_eq!(c.get_value(99), 0);
        // Past buffer end returns default.
        assert_eq!(c.get_value(100), 0);
    }

    #[test]
    fn test_set_region_single() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);
        assert_eq!(c.get_value(5), 0);
        assert_eq!(c.get_value(10), 1);
        assert_eq!(c.get_value(15), 1);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(20), 0);
        assert_eq!(c.get_value(50), 0);
    }

    #[test]
    fn test_set_region_at_start() {
        let mut c = make_cache(100);
        c.set_region(0, 10, 1);
        assert_eq!(c.get_value(0), 1);
        assert_eq!(c.get_value(9), 1);
        assert_eq!(c.get_value(10), 0);
    }

    #[test]
    fn test_set_region_at_end() {
        let mut c = make_cache(100);
        c.set_region(90, 100, 1);
        assert_eq!(c.get_value(89), 0);
        assert_eq!(c.get_value(90), 1);
        assert_eq!(c.get_value(99), 1);
    }

    #[test]
    fn test_set_region_entire_buffer() {
        let mut c = make_cache(100);
        c.set_region(0, 100, 1);
        for i in 0..100 {
            assert_eq!(c.get_value(i), 1, "pos {}", i);
        }
    }

    #[test]
    fn test_adjacent_merge() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);
        c.set_region(20, 30, 1);
        // The two regions should merge into one [10, 30).
        assert_eq!(c.get_value(10), 1);
        assert_eq!(c.get_value(25), 1);
        assert_eq!(c.get_value(29), 1);
        assert_eq!(c.get_value(30), 0);
        // sentinel(0:0), boundary(10:1), boundary(30:0) = 3 boundaries.
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn test_overlapping_set() {
        let mut c = make_cache(100);
        c.set_region(10, 30, 1);
        c.set_region(20, 40, 2);
        assert_eq!(c.get_value(10), 1);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(20), 2);
        assert_eq!(c.get_value(39), 2);
        assert_eq!(c.get_value(40), 0);
    }

    #[test]
    fn test_overwrite_region() {
        let mut c = make_cache(100);
        c.set_region(10, 50, 1);
        // Overwrite a sub-region back to default.
        c.set_region(20, 30, 0);
        assert_eq!(c.get_value(10), 1);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(20), 0);
        assert_eq!(c.get_value(29), 0);
        assert_eq!(c.get_value(30), 1);
        assert_eq!(c.get_value(49), 1);
        assert_eq!(c.get_value(50), 0);
    }

    #[test]
    fn test_invalidate_region() {
        let mut c = make_cache(100);
        c.set_region(10, 50, 1);
        c.invalidate_region(20, 30);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(20), 0);
        assert_eq!(c.get_value(29), 0);
        assert_eq!(c.get_value(30), 1);
    }

    #[test]
    fn test_invalidate_all() {
        let mut c = make_cache(100);
        c.set_region(0, 100, 1);
        c.invalidate_all();
        for i in 0..100 {
            assert_eq!(c.get_value(i), 0, "pos {}", i);
        }
    }

    #[test]
    fn test_forward_scan() {
        let mut c = make_cache(100);
        c.set_region(10, 30, 1);
        c.set_region(50, 70, 1);

        let (val, next) = c.forward(0);
        assert_eq!(val, 0);
        assert_eq!(next, 10);

        let (val, next) = c.forward(10);
        assert_eq!(val, 1);
        assert_eq!(next, 30);

        let (val, next) = c.forward(30);
        assert_eq!(val, 0);
        assert_eq!(next, 50);

        let (val, next) = c.forward(50);
        assert_eq!(val, 1);
        assert_eq!(next, 70);

        let (val, next) = c.forward(70);
        assert_eq!(val, 0);
        assert_eq!(next, 100);
    }

    #[test]
    fn test_backward_scan() {
        let mut c = make_cache(100);
        c.set_region(10, 30, 1);

        let (val, next) = c.backward(100);
        assert_eq!(val, 0);
        assert_eq!(next, 30);

        let (val, next) = c.backward(30);
        assert_eq!(val, 1);
        assert_eq!(next, 10);

        let (val, next) = c.backward(10);
        assert_eq!(val, 0);
        assert_eq!(next, 0);
    }

    #[test]
    fn test_backward_at_start() {
        let mut c = make_cache(100);
        let (val, next) = c.backward(0);
        assert_eq!(val, 0);
        assert_eq!(next, 0);
    }

    #[test]
    fn test_forward_at_end() {
        let mut c = make_cache(100);
        let (val, next) = c.forward(100);
        assert_eq!(val, 0);
        assert_eq!(next, 100);
    }

    #[test]
    fn test_adjust_for_insert_shifts_boundaries() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);

        // Insert 5 positions at position 15 (inside the known region).
        c.adjust_for_insert(15, 5);

        // The region [10, 15) in the unchanged head should still be known.
        // After revalidation, the inserted area is unknown.
        assert_eq!(c.get_value_revalidated(10), 1);
        assert_eq!(c.get_value(14), 1);
    }

    #[test]
    fn test_adjust_for_insert_before_region() {
        let mut c = make_cache(100);
        c.set_region(50, 60, 1);

        // Insert 10 positions at position 5 (before the known region).
        // Old buffer: [0..100], known: [50..60)
        // head=5 (unchanged prefix), tail=95 (100-5, unchanged suffix)
        // New buffer: [0..110]
        // After revalidation:
        //   - unchanged head: [0..5) still has old values (all 0)
        //   - modified region: [5..15) is unknown (default=0)
        //   - unchanged tail: the last 95 positions [15..110) float from
        //     old [5..100). Old 50 is at new 60, old 60 at new 70.
        c.adjust_for_insert(5, 10);

        // The known region should have shifted right by 10: [60, 70).
        assert_eq!(c.get_value_revalidated(59), 0);
        assert_eq!(c.get_value(60), 1);
        assert_eq!(c.get_value(69), 1);
        assert_eq!(c.get_value(70), 0);
    }

    #[test]
    fn test_adjust_for_delete() {
        let mut c = make_cache(100);
        c.set_region(50, 60, 1);

        // Delete positions [10, 20) (before the known region).
        // Old buffer: [0..100], known: [50..60)
        // head=10, tail=80 (100-20)
        // New buffer: [0..90]
        // After revalidation:
        //   - unchanged head: [0..10) still has old values (all 0)
        //   - modified region: [10..10) is empty (deletion shrinks it)
        //   - unchanged tail: the last 80 positions [10..90) float from
        //     old [20..100). Old 50 is at new 40, old 60 at new 50.
        c.adjust_for_delete(10, 20);

        assert_eq!(c.get_value_revalidated(39), 0);
        assert_eq!(c.get_value(40), 1);
        assert_eq!(c.get_value(49), 1);
        assert_eq!(c.get_value(50), 0);
    }

    #[test]
    fn test_know_region() {
        let mut c = make_cache(100);
        c.know_region(10, 20);
        assert_eq!(c.get_value(10), 1);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(9), 0);
        assert_eq!(c.get_value(20), 0);
    }

    #[test]
    fn test_multiple_values() {
        let mut c = make_cache(100);
        c.set_region(0, 25, 1);
        c.set_region(25, 50, 2);
        c.set_region(50, 75, 3);
        c.set_region(75, 100, 4);

        assert_eq!(c.get_value(0), 1);
        assert_eq!(c.get_value(24), 1);
        assert_eq!(c.get_value(25), 2);
        assert_eq!(c.get_value(49), 2);
        assert_eq!(c.get_value(50), 3);
        assert_eq!(c.get_value(74), 3);
        assert_eq!(c.get_value(75), 4);
        assert_eq!(c.get_value(99), 4);
    }

    #[test]
    fn test_many_small_regions() {
        let mut c = make_cache(200);
        for i in (0..200).step_by(4) {
            c.set_region(i, i + 2, 1);
        }
        for i in 0..200 {
            let expected = if i % 4 < 2 { 1 } else { 0 };
            assert_eq!(c.get_value(i), expected, "pos {}", i);
        }
    }

    #[test]
    fn test_set_region_empty_is_noop() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);
        let len_before = c.len();
        c.set_region(15, 15, 2); // empty region
        assert_eq!(c.len(), len_before);
        assert_eq!(c.get_value(15), 1);
    }

    #[test]
    fn test_set_same_value_merges() {
        let mut c = make_cache(100);
        c.set_region(0, 30, 1);
        c.set_region(30, 70, 1);
        c.set_region(70, 100, 1);
        // Should have just the sentinel with value 1.
        assert_eq!(c.len(), 1);
        for i in 0..100 {
            assert_eq!(c.get_value(i), 1, "pos {}", i);
        }
    }

    #[test]
    fn test_debug_format() {
        let mut c = make_cache(50);
        c.set_region(10, 20, 1);
        let debug_str = format!("{:?}", c);
        assert!(debug_str.contains("RegionCache"));
        assert!(debug_str.contains("buffer: 0..50"));
    }

    #[test]
    fn test_boundary_count_after_operations() {
        let mut c = make_cache(100);
        assert_eq!(c.len(), 1);

        c.set_region(10, 20, 1);
        // sentinel(0:0), boundary(10:1), boundary(20:0) = 3
        assert_eq!(c.len(), 3);

        c.set_region(20, 30, 1);
        // Merges: sentinel(0:0), boundary(10:1), boundary(30:0) = 3
        assert_eq!(c.len(), 3);

        c.set_region(0, 10, 1);
        // Merges with sentinel: sentinel(0:1), boundary(30:0) = 2
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn test_adjust_for_insert_at_buffer_end() {
        let mut c = make_cache(100);
        c.set_region(90, 100, 1);

        // Insert at the very end.
        c.adjust_for_insert(100, 10);

        // The old region [90, 100) should still be known.
        assert_eq!(c.get_value_revalidated(90), 1);
        assert_eq!(c.get_value(99), 1);
        // The newly inserted area [100, 110) is unknown.
        assert_eq!(c.get_value(100), 0);
    }

    #[test]
    fn test_stress_alternating_set_invalidate() {
        let mut c = make_cache(1000);
        for round in 0..20 {
            let start = (round * 47) % 900;
            let end = start + 50;
            c.set_region(start, end, 1);
            let inv_start = (round * 31) % 900;
            let inv_end = inv_start + 30;
            c.invalidate_region(inv_start, inv_end);
        }
        // Verify it doesn't panic and produces valid results.
        for i in 0..1000 {
            let v = c.get_value(i);
            assert!(v == 0 || v == 1, "unexpected value {} at pos {}", v, i);
        }
    }

    #[test]
    fn test_adjust_for_delete_at_end() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);

        // Delete last 10 positions [90, 100).
        c.adjust_for_delete(90, 100);

        // The known region should be unaffected.
        assert_eq!(c.get_value_revalidated(10), 1);
        assert_eq!(c.get_value(19), 1);
        assert_eq!(c.get_value(20), 0);
    }

    #[test]
    fn test_adjust_for_insert_then_set() {
        let mut c = make_cache(100);
        c.set_region(10, 20, 1);

        // Insert 5 at position 25.
        c.adjust_for_insert(25, 5);

        // Set a new region in the expanded buffer.
        c.set_region(30, 40, 2);

        assert_eq!(c.get_value(30), 2);
        assert_eq!(c.get_value(39), 2);
        assert_eq!(c.get_value(40), 0);
    }

    #[test]
    fn test_adjust_for_delete_overlapping_known() {
        let mut c = make_cache(100);
        c.set_region(40, 60, 1);

        // Delete [45, 55): this overlaps with the known region.
        c.adjust_for_delete(45, 55);

        // After deletion, buffer is [0..90].
        // Unchanged head: [0..45), unchanged tail: last 45 positions [45..90).
        // The modified region in the new buffer is [45..45) -- empty.
        // Old [40..45) is in unchanged head, still known.
        assert_eq!(c.get_value_revalidated(40), 1);
        assert_eq!(c.get_value(44), 1);
        // Old [55..60) floats to new [45..50), still known.
        assert_eq!(c.get_value(45), 1);
        assert_eq!(c.get_value(49), 1);
        assert_eq!(c.get_value(50), 0);
    }
}
