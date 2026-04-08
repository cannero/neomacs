use core::sync::atomic::{AtomicU8, Ordering};
use std::collections::{HashMap, HashSet};

use crate::card_table::CardTable;
use crate::object::{ObjectRecord, OldBlockPlacement, OldRegionPlacement, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::reclaim::PreparedReclaimSurvivor;
use crate::stats::OldRegionStats;

/// Old-generation configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OldGenConfig {
    /// Region size in bytes.
    pub region_bytes: usize,
    /// Line size in bytes for occupancy tracking.
    pub line_bytes: usize,
    /// Maximum number of old regions to target in one planned compaction cycle.
    pub compaction_candidate_limit: usize,
    /// Minimum reclaimable bytes required for a region to become a compaction candidate.
    pub selective_reclaim_threshold_bytes: usize,
    /// Maximum live bytes selected for compaction in one planned cycle.
    pub max_compaction_bytes_per_cycle: usize,
    /// Maximum number of concurrent mark workers.
    pub concurrent_mark_workers: usize,
    /// Number of major-mark slices one mutator operation should assist.
    pub mutator_assist_slices: usize,
    /// Density threshold below which an `OldBlock` becomes a
    /// physical-compaction candidate during a major cycle.
    /// Expressed as a ratio in `[0.0, 1.0]`. A block whose
    /// post-mark `live_bytes / capacity_bytes` is at or below
    /// this threshold has every surviving record evacuated into
    /// a freshly-created target block, after which the now-empty
    /// source block is reclaimed by the block-pool sweep.
    ///
    /// The default is `0.0` — physical compaction is opt-in.
    /// At `0.0` the threshold never fires (density is always
    /// `> 0.0` for blocks that still hold live records), so the
    /// major reclaim pipeline runs the legacy logical-compaction
    /// path unchanged. Setting this to e.g. `0.3` enables
    /// physical compaction of any block that is 30% full or less.
    pub physical_compaction_density_threshold: f64,
}

impl Default for OldGenConfig {
    fn default() -> Self {
        Self {
            region_bytes: 4 * 1024 * 1024,
            line_bytes: 256,
            compaction_candidate_limit: 8,
            selective_reclaim_threshold_bytes: 1,
            max_compaction_bytes_per_cycle: usize::MAX,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 1,
            physical_compaction_density_threshold: 0.0,
        }
    }
}

/// A single old-generation Immix-style block.
///
/// Each block owns a contiguous backing buffer divided into fixed-size
/// lines. The block tracks per-line occupancy with `line_marks` so the
/// post-sweep allocator can find runs of free lines (Immix hole filling)
/// before falling back to a fresh block.
///
/// `cursor` is a hint into the byte buffer where the next allocation scan
/// starts. After a sweep the cursor is reset to zero so the allocator can
/// see freshly opened holes near the front of the block.
#[derive(Debug)]
pub(crate) struct OldBlock {
    buffer: Box<[u8]>,
    line_marks: Box<[AtomicU8]>,
    line_bytes: usize,
    cursor: usize,
    /// High-water mark: the largest offset any allocation has ever
    /// advanced the cursor to. Tracks the logical "region used bytes"
    /// concept that Step 1 of the OldRegion → OldBlock unification is
    /// lifting out of `OldRegion`. Reset to 0 when the block is
    /// cleared (currently unused — blocks are dropped, not cleared,
    /// on reclaim — but preserved for future sweep paths).
    used_bytes: usize,
    /// Sum of `total_size` over every live object currently placed
    /// in the block. Mirrors `OldRegion::live_bytes`. Updated by
    /// `record_object_accounting` and cleared by
    /// `clear_live_accounting`.
    live_bytes: usize,
    /// Count of live objects currently placed in the block. Mirrors
    /// `OldRegion::object_count`. Updated alongside `live_bytes`.
    object_count: usize,
    /// Set of line indices that contain at least one live object.
    /// Distinct from `line_marks`: the atomic `line_marks` array is
    /// set only during sweep (by the post-sweep survivor walk), while
    /// this `HashSet` mirrors the exact semantics of
    /// `OldRegion::occupied_lines` — updated at allocation time by
    /// `record_object_accounting` so `region_stats` can report
    /// `occupied_lines` without a sweep having run yet. A later
    /// consolidation can fold this into `line_marks` once the
    /// sweep path is also migrated.
    occupied_lines: HashSet<usize>,
    /// Per-block card table covering the backing buffer. The write barrier
    /// dirties cards through this table when the owner of an old-to-young
    /// edge lives inside the block; the minor GC root scan walks the dirty
    /// cards to find old-gen objects that may reference young targets.
    card_table: CardTable,
    /// One entry per card in this block. Each entry is the offset (from
    /// the start of the buffer) of the FIRST object header that lies in
    /// that card. `None` if no object starts in that card. The minor GC
    /// dirty-card root scan uses this index to walk dirty cards in
    /// O(dirty_cards) instead of doing a linear pass over every record
    /// in `Heap::objects` per dirty card. Subsequent objects in the same
    /// card are reached by walking forward from the first one via the
    /// per-object total_size header field.
    object_starts: Box<[Option<u32>]>,
}

impl OldBlock {
    /// Construct a new block whose backing buffer is at least
    /// `capacity_bytes` long, rounded up to a whole number of `line_bytes`
    /// lines (and at least one line so degenerate configurations stay
    /// well-defined).
    pub(crate) fn new(capacity_bytes: usize, line_bytes: usize) -> Self {
        let line_bytes = line_bytes.max(1);
        let line_count = capacity_bytes.div_ceil(line_bytes).max(1);
        let buffer_len = line_count.saturating_mul(line_bytes);
        let buffer: Box<[u8]> = vec![0u8; buffer_len].into_boxed_slice();
        let mut marks = Vec::with_capacity(line_count);
        for _ in 0..line_count {
            marks.push(AtomicU8::new(0));
        }
        let base_addr = buffer.as_ptr() as usize;
        let card_table = CardTable::with_default_card_size(base_addr, buffer_len);
        let object_starts = vec![None; card_table.card_count()].into_boxed_slice();
        Self {
            buffer,
            line_marks: marks.into_boxed_slice(),
            line_bytes,
            cursor: 0,
            used_bytes: 0,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
            card_table,
            object_starts,
        }
    }

    /// Accumulated size of every live object currently placed in
    /// this block. Mirrors `OldRegion::live_bytes`. Updated by
    /// [`record_object_accounting`] and reset by
    /// [`clear_live_accounting`].
    pub(crate) fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    /// Count of live objects currently placed in this block.
    /// Mirrors `OldRegion::object_count`. Updated by
    /// [`record_object_accounting`] and reset by
    /// [`clear_live_accounting`].
    pub(crate) fn object_count(&self) -> usize {
        self.object_count
    }

    /// Allocation high-water mark: the largest buffer offset any
    /// `try_alloc` has advanced through. Mirrors
    /// `OldRegion::used_bytes`. Distinct from [`cursor`] because
    /// sweep resets the cursor for hole-filling but keeps
    /// `used_bytes` as the upper bound of ever-allocated space
    /// (useful for stats and compaction planning).
    pub(crate) fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Number of lines currently containing at least one live
    /// object, as tracked by the allocation-time `occupied_lines`
    /// set. Distinct from [`line_marks`]: this reflects what
    /// `record_object_accounting` has recorded, while `line_marks`
    /// is populated only during post-sweep rebuild.
    pub(crate) fn occupied_line_count(&self) -> usize {
        self.occupied_lines.len()
    }

    /// Record a newly-placed live object in the block's accounting
    /// counters. Bumps `live_bytes`, `object_count`, lifts
    /// `used_bytes` to cover the object's tail, and inserts every
    /// line the object overlaps into the `occupied_lines` set. This
    /// mirrors the exact semantics of the legacy
    /// `OldRegion::live_bytes` / `object_count` / `occupied_lines`
    /// accounting so `region_stats()` can be migrated to compute
    /// from blocks in step 4 without any behavior change.
    pub(crate) fn record_object_accounting(&mut self, offset: usize, size: usize) {
        if size == 0 {
            return;
        }
        self.live_bytes = self.live_bytes.saturating_add(size);
        self.object_count = self.object_count.saturating_add(1);
        let end = offset.saturating_add(size);
        if end > self.used_bytes {
            self.used_bytes = end.min(self.buffer.len());
        }
        let first_line = offset / self.line_bytes;
        let last_byte = end.saturating_sub(1);
        let last_line = last_byte / self.line_bytes;
        for line in first_line..=last_line {
            self.occupied_lines.insert(line);
        }
    }

    /// Clear the live-object accounting counters without touching
    /// the backing buffer or line marks. Invoked at the start of a
    /// sweep-driven rebuild, right before the survivor walk
    /// re-populates them via [`record_object_accounting`]. Leaves
    /// `used_bytes` at its current high-water mark — the block's
    /// physical layout does not shrink even after dead objects are
    /// reclaimed.
    pub(crate) fn clear_live_accounting(&mut self) {
        self.live_bytes = 0;
        self.object_count = 0;
        self.occupied_lines.clear();
    }

    /// Read-only view of the per-card object-start index. Each entry is
    /// the byte offset (relative to the buffer base) of the first object
    /// header that lies in that card, or `None` if no object starts in
    /// the card. Used by the minor GC dirty-card scan and by tests.
    pub(crate) fn object_starts(&self) -> &[Option<u32>] {
        &self.object_starts
    }

    /// Reset every per-card object-start entry to `None`. Called from
    /// the post-sweep rebuild before walking surviving records to repopulate
    /// the index.
    pub(crate) fn clear_object_starts(&mut self) {
        for slot in self.object_starts.iter_mut() {
            *slot = None;
        }
    }

    /// Record an object start at the given buffer offset. The per-card
    /// object-start entry tracks the SMALLEST offset whose header lies
    /// in that card; subsequent objects in the same card can be discovered
    /// by walking forward via the per-object total_size header field.
    /// We track the smallest offset (rather than the first call to win)
    /// because the post-sweep rebuild may visit surviving records out of
    /// allocation order. Out-of-range offsets are silently ignored.
    pub(crate) fn record_object_start(&mut self, offset: usize) {
        let card_size = self.card_table.card_size();
        let card_idx = offset / card_size;
        let offset_u32 = offset as u32;
        if let Some(slot) = self.object_starts.get_mut(card_idx) {
            match slot {
                None => *slot = Some(offset_u32),
                Some(existing) if offset_u32 < *existing => *slot = Some(offset_u32),
                _ => {}
            }
        }
    }

    /// Read-only access to the per-block card table. The remembered-set
    /// write barrier and the minor GC root scan use this to dirty/scan
    /// cards covering the block buffer.
    pub(crate) fn card_table(&self) -> &CardTable {
        &self.card_table
    }

    /// Total backing buffer length in bytes.
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Number of lines in the block.
    pub(crate) fn line_count(&self) -> usize {
        self.line_marks.len()
    }

    /// Bytes per line.
    pub(crate) fn line_bytes(&self) -> usize {
        self.line_bytes
    }

    /// Base pointer of the backing buffer (read-only). The pointer remains
    /// valid for the lifetime of the block.
    pub(crate) fn base_ptr(&self) -> *const u8 {
        self.buffer.as_ptr()
    }

    /// True if the byte at `addr` (an absolute pointer-as-usize) lies
    /// inside this block's backing buffer.
    pub(crate) fn contains_addr(&self, addr: usize) -> bool {
        let base = self.base_ptr() as usize;
        addr >= base && addr < base + self.buffer.len()
    }

    /// Mark the line at `index` as occupied. Out-of-range indices are
    /// silently ignored.
    pub(crate) fn mark_line(&self, index: usize) {
        if let Some(slot) = self.line_marks.get(index) {
            slot.store(1, Ordering::Relaxed);
        }
    }

    /// Test whether the line at `index` is currently marked occupied.
    pub(crate) fn is_line_marked(&self, index: usize) -> bool {
        self.line_marks
            .get(index)
            .map(|slot| slot.load(Ordering::Relaxed) != 0)
            .unwrap_or(false)
    }

    /// Mark every line covered by the byte range `[offset, offset + size)`
    /// as occupied. Sweep walks surviving block-backed records and calls
    /// this for each one to rebuild the line occupancy map.
    pub(crate) fn mark_lines_for_range(&self, offset: usize, size: usize) {
        if size == 0 {
            return;
        }
        let start_line = offset / self.line_bytes;
        let end_byte = offset.saturating_add(size).saturating_sub(1);
        let end_line = end_byte / self.line_bytes;
        let last_line = self.line_count().saturating_sub(1);
        let end_line = end_line.min(last_line);
        for line in start_line..=end_line {
            self.mark_line(line);
        }
    }

    /// Clear every line mark in the block.
    pub(crate) fn clear_line_marks(&self) {
        for slot in self.line_marks.iter() {
            slot.store(0, Ordering::Relaxed);
        }
    }

    /// True when no line is marked as occupied. Empty blocks are reclaimed
    /// after the sweep.
    pub(crate) fn is_empty(&self) -> bool {
        self.line_marks
            .iter()
            .all(|slot| slot.load(Ordering::Relaxed) == 0)
    }

    /// Reset the bump cursor back to the start of the block.
    pub(crate) fn reset_cursor(&mut self) {
        self.cursor = 0;
    }

    /// Try to allocate `layout.size()` bytes from the block using
    /// hole-filling. The implementation scans `line_marks` starting at
    /// the current cursor for the first run of `ceil(size / line_bytes)`
    /// consecutive free lines. On success the cursor advances past the
    /// allocation and the function returns the offset of the placement
    /// inside the buffer plus a `NonNull<u8>` to that slot.
    pub(crate) fn try_alloc(
        &mut self,
        layout: core::alloc::Layout,
    ) -> Option<(usize, core::ptr::NonNull<u8>)> {
        let size = layout.size();
        if size == 0 {
            return None;
        }
        if size > self.buffer.len() {
            return None;
        }
        let lines_needed = size.div_ceil(self.line_bytes).max(1);
        let line_count = self.line_count();
        if lines_needed > line_count {
            return None;
        }

        let cursor_line = self.cursor.div_ceil(self.line_bytes);
        let mut search_line = cursor_line;
        while search_line + lines_needed <= line_count {
            // Skip over any occupied lines to reach the next free run.
            while search_line + lines_needed <= line_count
                && self.is_line_marked(search_line)
            {
                search_line += 1;
            }
            if search_line + lines_needed > line_count {
                break;
            }
            // Check whether `lines_needed` consecutive lines are free here.
            let mut run_end = search_line;
            while run_end < line_count
                && !self.is_line_marked(run_end)
                && run_end - search_line < lines_needed
            {
                run_end += 1;
            }
            if run_end - search_line >= lines_needed {
                let offset = search_line * self.line_bytes;
                let alloc_end = offset + size;
                if alloc_end > self.buffer.len() {
                    return None;
                }
                // Honour the requested alignment if it exceeds line alignment.
                // Line starts are at line_bytes-multiples of the buffer base
                // pointer, so most use cases are already covered, but a tiny
                // re-check guards against pathological alignment requests.
                let base_addr = self.buffer.as_ptr() as usize;
                let slot_addr = base_addr + offset;
                if !slot_addr.is_multiple_of(layout.align().max(1)) {
                    // The requested alignment exceeds line alignment; skip
                    // this run and keep searching.
                    search_line = run_end;
                    continue;
                }
                let after_lines = offset + lines_needed * self.line_bytes;
                self.cursor = after_lines.min(self.buffer.len());
                if self.cursor > self.used_bytes {
                    self.used_bytes = self.cursor;
                }
                // Maintain the per-card object-start index. The first
                // object in each card stays as the index entry, so we
                // only update the slot when it is currently empty.
                self.record_object_start(offset);
                // SAFETY: offset is in-range; the buffer outlives the block.
                let raw = unsafe { (self.buffer.as_ptr() as *mut u8).add(offset) };
                let ptr = core::ptr::NonNull::new(raw)?;
                return Some((offset, ptr));
            }
            search_line = run_end;
        }
        None
    }
}

#[derive(Debug, Default)]
pub(crate) struct OldGenState {
    pub(crate) regions: Vec<OldRegion>,
    /// Block buffer pool. Blocks are allocated on demand when direct old-gen
    /// allocation or nursery promotion needs fresh backing storage, and the
    /// post-sweep reclaim path drops blocks whose line marks are entirely
    /// empty (Immix-style block reclaim).
    blocks: Vec<OldBlock>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OldGenPlanSelection {
    pub(crate) candidates: Vec<OldRegionStats>,
    pub(crate) estimated_compaction_bytes: usize,
    pub(crate) estimated_reclaim_bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PreparedOldGenReclaim {
    pub(crate) rebuilt_regions: Vec<OldRegion>,
    pub(crate) reserved_bytes: usize,
    pub(crate) region_stats: OldRegionCollectionStats,
}

impl OldGenState {
    pub(crate) fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub(crate) fn reserved_bytes(&self) -> usize {
        self.regions
            .iter()
            .map(|region| region.capacity_bytes)
            .sum()
    }

    pub(crate) fn allocate_placement(
        &mut self,
        config: &OldGenConfig,
        bytes: usize,
    ) -> OldRegionPlacement {
        let align = config.line_bytes.max(8);
        if let Some((region_index, offset)) = self.try_reserve_in_existing_region(bytes, align) {
            return self.make_placement(config, region_index, offset, bytes);
        }

        let capacity_bytes = config.region_bytes.max(bytes);
        self.regions.push(OldRegion {
            capacity_bytes,
            used_bytes: 0,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
        let region_index = self.regions.len() - 1;
        let offset = self.regions[region_index].used_bytes;
        self.regions[region_index].used_bytes = bytes;
        self.make_placement(config, region_index, offset, bytes)
    }

    /// Phase 2 Immix-style block allocation. Walks every block looking for
    /// a hole large enough to fit `layout` (hole-filling), and on failure
    /// allocates a fresh block sized to the larger of `config.region_bytes`
    /// and `layout.size()`. Returns the placement (block index plus byte
    /// offset) and a `NonNull<u8>` to the placement slot.
    pub(crate) fn try_alloc_in_block(
        &mut self,
        config: &OldGenConfig,
        layout: core::alloc::Layout,
    ) -> Option<(OldBlockPlacement, core::ptr::NonNull<u8>)> {
        // Try every existing block from oldest to newest. Hot allocation
        // benefits from staying in the most recently used block first, but
        // hole filling improves overall density at the cost of one extra
        // pass — start the search at the beginning so we always re-use the
        // earliest available hole, mirroring the Immix paper recommendation.
        for index in 0..self.blocks.len() {
            if let Some((offset, ptr)) = self.blocks[index].try_alloc(layout) {
                let placement = OldBlockPlacement {
                    block_index: index,
                    offset_bytes: offset,
                    total_size: layout.size(),
                };
                return Some((placement, ptr));
            }
        }

        // No existing block had room — allocate a new block sized to
        // the larger of the configured region size and the requested
        // layout.
        let capacity = config.region_bytes.max(layout.size());
        let line_bytes = config.line_bytes.max(1);
        let mut block = OldBlock::new(capacity, line_bytes);
        let (offset, ptr) = block.try_alloc(layout)?;
        let block_index = self.blocks.len();
        self.blocks.push(block);
        Some((
            OldBlockPlacement {
                block_index,
                offset_bytes: offset,
                total_size: layout.size(),
            },
            ptr,
        ))
    }

    /// Compaction target allocator: try to place `layout` into the
    /// block at `target_hint`; if that fails (or `target_hint` is
    /// `None`), create a fresh block and place the layout there.
    /// Returns the `(placement, pointer, new_target_hint)` tuple;
    /// the new_target_hint is the block index the allocation
    /// landed in, which the caller should thread into the next
    /// call so multiple survivors from the compaction pass share
    /// the same target block.
    ///
    /// Returns `None` only if even the fresh-block path cannot
    /// service the layout (e.g. `layout.size() == 0`). Callers
    /// treat that as "skip this survivor."
    pub(crate) fn alloc_for_compaction_into_target(
        &mut self,
        config: &OldGenConfig,
        layout: core::alloc::Layout,
        target_hint: Option<usize>,
    ) -> Option<(OldBlockPlacement, core::ptr::NonNull<u8>, usize)> {
        if let Some(index) = target_hint
            && let Some(block) = self.blocks.get_mut(index)
                && let Some((offset, ptr)) = block.try_alloc(layout) {
                    return Some((
                        OldBlockPlacement {
                            block_index: index,
                            offset_bytes: offset,
                            total_size: layout.size(),
                        },
                        ptr,
                        index,
                    ));
                }
        let (placement, ptr) = self.alloc_in_fresh_block(config, layout)?;
        let new_target = placement.block_index;
        Some((placement, ptr, new_target))
    }

    /// Allocate directly into a newly-created block, bypassing the
    /// hole-filling search over existing blocks that
    /// [`try_alloc_in_block`] performs.
    ///
    /// Used by the upcoming physical-compaction pass: compaction
    /// evacuates survivors from sparse source blocks into fresh
    /// target blocks, and must never accidentally pick one of the
    /// source blocks itself as the target. `alloc_in_fresh_block`
    /// side-steps that risk by always creating a brand-new block
    /// sized to the larger of `config.region_bytes` and
    /// `layout.size()`.
    ///
    /// Returns `None` if the underlying `OldBlock::try_alloc` fails
    /// on the fresh block (e.g. layout overflow); in the normal case
    /// a brand-new block is guaranteed to have room.
    pub(crate) fn alloc_in_fresh_block(
        &mut self,
        config: &OldGenConfig,
        layout: core::alloc::Layout,
    ) -> Option<(OldBlockPlacement, core::ptr::NonNull<u8>)> {
        let capacity = config.region_bytes.max(layout.size());
        let line_bytes = config.line_bytes.max(1);
        let mut block = OldBlock::new(capacity, line_bytes);
        let (offset, ptr) = block.try_alloc(layout)?;
        let block_index = self.blocks.len();
        self.blocks.push(block);
        Some((
            OldBlockPlacement {
                block_index,
                offset_bytes: offset,
                total_size: layout.size(),
            },
            ptr,
        ))
    }

    /// Number of physical blocks currently in the pool.
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Iterate over every block in the pool.
    pub(crate) fn blocks(&self) -> &[OldBlock] {
        &self.blocks
    }

    /// Find the index of the block whose backing buffer contains `addr`.
    /// Naive linear scan over the block pool — fine for the small block
    /// counts the GC currently holds. The minor write barrier consults
    /// this on every old-to-young edge mutation, so a future optimization
    /// could replace this with an interval-tree or sorted-base lookup.
    pub(crate) fn find_block_for_addr(&self, addr: usize) -> Option<usize> {
        for (index, block) in self.blocks.iter().enumerate() {
            if block.contains_addr(addr) {
                return Some(index);
            }
        }
        None
    }

    /// Mark the card containing `owner_addr` as dirty if `owner_addr`
    /// falls inside one of the old-gen blocks. Returns true if a card
    /// was set; false when the owner is not block-backed (e.g. it lives
    /// in a pinned record, large-object space, or a system-allocated
    /// fallback record). Callers fall back to the legacy
    /// `RememberedSetState` for the false case.
    pub(crate) fn record_write_barrier(&self, owner_addr: usize) -> bool {
        let Some(index) = self.find_block_for_addr(owner_addr) else {
            return false;
        };
        self.blocks[index].card_table().record_write(owner_addr);
        true
    }

    /// Reset every per-block card table back to clean. Invoked at the
    /// end of a minor collection after the dirty-card scan has produced
    /// roots for the trace.
    pub(crate) fn clear_all_dirty_cards(&self) {
        for block in &self.blocks {
            block.card_table().clear_all();
        }
    }

    /// Total number of dirty cards across every block in the pool. Used
    /// by tests to assert the write-barrier and post-collection clear
    /// behaviors.
    pub(crate) fn dirty_card_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.card_table().dirty_card_indices().len())
            .sum()
    }

    /// Clear every line mark across every block. Called at the start of
    /// the post-sweep rebuild so the survivor walk can re-mark only the
    /// lines that still hold live objects.
    pub(crate) fn clear_all_block_line_marks(&self) {
        for block in &self.blocks {
            block.clear_line_marks();
        }
    }

    /// Clear every per-card object-start entry across every block. The
    /// post-sweep rebuild calls this before walking surviving block-
    /// backed records to repopulate the index from the new layout.
    pub(crate) fn clear_all_block_object_starts(&mut self) {
        for block in &mut self.blocks {
            block.clear_object_starts();
        }
    }

    /// Clear the live-object accounting counters (`live_bytes`,
    /// `object_count`, `occupied_lines`) on every block in the
    /// pool. Invoked at the start of the post-sweep rebuild so the
    /// survivor walk can repopulate the counters from the new
    /// layout via `record_block_object_accounting_for_placement`.
    pub(crate) fn clear_all_block_live_accounting(&mut self) {
        for block in &mut self.blocks {
            block.clear_live_accounting();
        }
    }

    /// Record a surviving block-backed object into the block's
    /// live accounting counters (`live_bytes`, `object_count`,
    /// `occupied_lines`). Mirrors
    /// `record_block_object_start_for_placement` and
    /// `mark_block_lines_for_placement`: these three helpers are
    /// called in lockstep by the post-sweep rebuild for every
    /// surviving record.
    pub(crate) fn record_block_object_accounting_for_placement(
        &mut self,
        placement: OldBlockPlacement,
    ) {
        if let Some(block) = self.blocks.get_mut(placement.block_index) {
            block.record_object_accounting(placement.offset_bytes, placement.total_size);
        }
    }

    /// Record an object start in the block identified by `placement`.
    /// Used by the post-sweep rebuild to repopulate the per-card
    /// object-start index from surviving records.
    pub(crate) fn record_block_object_start_for_placement(
        &mut self,
        placement: OldBlockPlacement,
    ) {
        if let Some(block) = self.blocks.get_mut(placement.block_index) {
            block.record_object_start(placement.offset_bytes);
        }
    }

    /// Mark the lines covered by `placement` in the corresponding block.
    pub(crate) fn mark_block_lines_for_placement(&self, placement: OldBlockPlacement) {
        if let Some(block) = self.blocks.get(placement.block_index) {
            block.mark_lines_for_range(placement.offset_bytes, placement.total_size);
        }
    }

    /// Reset every block's bump cursor without dropping any blocks. This is
    /// called at the start of the post-sweep allocation cycle so the next
    /// allocation walks line marks from offset 0.
    pub(crate) fn reset_block_cursors(&mut self) {
        for block in &mut self.blocks {
            block.reset_cursor();
        }
    }

    /// Compute a remapping that drops empty blocks and rewrites surviving
    /// indices into a contiguous 0..N range. Returns `(new_indices, dropped)`
    /// where `new_indices[old] == Some(new)` if the block survives or `None`
    /// if it was dropped.
    pub(crate) fn compute_block_index_remap(&self) -> (Vec<Option<usize>>, usize) {
        let mut new_indices = Vec::with_capacity(self.blocks.len());
        let mut next = 0usize;
        let mut dropped = 0usize;
        for block in &self.blocks {
            if block.is_empty() {
                new_indices.push(None);
                dropped += 1;
            } else {
                new_indices.push(Some(next));
                next += 1;
            }
        }
        (new_indices, dropped)
    }

    /// Drop blocks whose line marks are completely empty after a sweep.
    /// Returns a remap from old block indices to new block indices (or
    /// `None` if the block was dropped) so the caller can rebind any
    /// surviving `OldBlockPlacement::block_index` values that were stored
    /// outside the block pool.
    ///
    /// IMPORTANT: callers must mark the lines of every record that anchors
    /// a block — including pending finalizers — before invoking this
    /// function. A block is reclaimed iff none of its lines are marked.
    pub(crate) fn drop_unused_blocks_with_remap(&mut self) -> Vec<Option<usize>> {
        let (remap, dropped) = self.compute_block_index_remap();
        if dropped == 0 {
            self.reset_block_cursors();
            return remap;
        }

        let mut next = 0usize;
        let mut keep_mask = Vec::with_capacity(self.blocks.len());
        for entry in &remap {
            keep_mask.push(entry.is_some());
        }
        self.blocks.retain(|_| {
            let keep = keep_mask[next];
            next += 1;
            keep
        });

        // Reset cursors on the surviving blocks so newly opened holes are
        // visible to the next allocation cycle.
        self.reset_block_cursors();

        remap
    }

    pub(crate) fn record_object(&mut self, object: &ObjectRecord) {
        // Block-side accounting. Every promoted or direct-allocated
        // old-gen record goes through `try_alloc_in_block` for its
        // physical bytes, so it has an `OldBlockPlacement` set by
        // the time `record_object` is invoked. Update the block's
        // live_bytes / object_count counters so step 4 of the
        // OldRegion unification can read region-stats from blocks.
        if let Some(block_placement) = object.old_block_placement()
            && let Some(block) = self.blocks.get_mut(block_placement.block_index) {
                block.record_object_accounting(
                    block_placement.offset_bytes,
                    object.total_size(),
                );
            }

        // Legacy region-side accounting. Still maintained in this
        // step so every existing reader (region_stats,
        // major_plan_selection, rebuild) observes the same numbers
        // as before. Later steps move the readers to the block
        // side and delete this branch.
        let Some(placement) = object.old_region_placement() else {
            return;
        };
        let region = &mut self.regions[placement.region_index];
        region.live_bytes = region.live_bytes.saturating_add(object.total_size());
        region.object_count = region.object_count.saturating_add(1);
        for line in placement.line_start..placement.line_start + placement.line_count {
            region.occupied_lines.insert(line);
        }
    }

    pub(crate) fn record_allocated_object(
        &mut self,
        config: &OldGenConfig,
        object: &mut ObjectRecord,
    ) -> usize {
        let placement = self.allocate_placement(config, object.total_size());
        object.set_old_region_placement(placement);
        self.record_object(object);
        self.reserved_bytes()
    }

    pub(crate) fn region_stats(&self) -> Vec<OldRegionStats> {
        // Still reads from the legacy `regions` vec. An earlier
        // attempt to compute this from blocks (step 10) tripped
        // `execute_major_plan_honors_exact_selected_old_regions`
        // because that test asserts the logical-compaction
        // behavior of `hole_bytes` shrinking after a major --
        // which happens when the legacy rebuild path rewrites
        // region offsets tight against survivors, but does NOT
        // happen when computing the same metric from blocks (the
        // block's `used_bytes` stays at its physical high-water
        // mark). The test semantic is fundamentally a logical-
        // compaction contract, so as long as the test survives
        // the regions vec has to be the source of truth.
        //
        // The block side does maintain identical counters
        // (updated by record_object on alloc and by the sweep
        // rebuild on survivors) so that future step can switch
        // the reader if the test contract is revised.
        self.regions
            .iter()
            .enumerate()
            .map(|(region_index, region)| OldRegionStats {
                region_index,
                reserved_bytes: region.capacity_bytes,
                used_bytes: region.used_bytes,
                live_bytes: region.live_bytes,
                free_bytes: region.capacity_bytes.saturating_sub(region.live_bytes),
                hole_bytes: region.used_bytes.saturating_sub(region.live_bytes),
                tail_bytes: region.capacity_bytes.saturating_sub(region.used_bytes),
                object_count: region.object_count,
                occupied_lines: region.occupied_lines.len(),
            })
            .collect()
    }

    /// Block-backed stats view. Each `OldBlock` maps to one
    /// `OldRegionStats` entry (using the block index as the
    /// pseudo region index). This exposes the per-block
    /// counters maintained by the sweep rebuild without going
    /// through the legacy `regions` vec.
    ///
    /// This is the long-term replacement for
    /// [`OldGenState::region_stats`]: once the logical-region
    /// `hole_bytes` shrink contract is migrated off the
    /// remaining `lib_test.rs` assertions, the legacy `regions`
    /// vec and `region_stats()` go away and this becomes the
    /// only reader. Until then both views ship in parallel and
    /// callers that want the honest physical layout (e.g.
    /// post-physical-compaction observers) should read this.
    ///
    /// Field semantics:
    /// * `region_index` — block index in allocation order.
    /// * `reserved_bytes` — block capacity.
    /// * `used_bytes` — high-water mark of the bump cursor in
    ///   the block. Does NOT shrink under logical compaction.
    /// * `live_bytes` — sum of survivor sizes after the most
    ///   recent sweep rebuild.
    /// * `hole_bytes` — interior gaps (`used_bytes - live_bytes`).
    /// * `tail_bytes` — unused tail at the end of the block.
    /// * `object_count` — number of survivors in the block.
    /// * `occupied_lines` — number of Immix lines containing
    ///   live data.
    pub(crate) fn block_region_stats(&self) -> Vec<OldRegionStats> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let reserved_bytes = block.capacity_bytes();
                let used_bytes = block.used_bytes();
                let live_bytes = block.live_bytes();
                OldRegionStats {
                    region_index: index,
                    reserved_bytes,
                    used_bytes,
                    live_bytes,
                    free_bytes: reserved_bytes.saturating_sub(live_bytes),
                    hole_bytes: used_bytes.saturating_sub(live_bytes),
                    tail_bytes: reserved_bytes.saturating_sub(used_bytes),
                    object_count: block.object_count(),
                    occupied_lines: block.occupied_line_count(),
                }
            })
            .collect()
    }

    pub(crate) fn major_plan_selection(&self, config: &OldGenConfig) -> OldGenPlanSelection {
        Self::run_major_plan_selection(self.region_stats(), config)
    }

    /// Block-backed equivalent of [`Self::major_plan_selection`].
    /// Runs the identical hole-bytes heuristic but reads candidate
    /// stats from the per-block view ([`Self::block_region_stats`])
    /// instead of the legacy regions vec. Returned `region_index`
    /// fields refer to block indices.
    ///
    /// Unlike [`Self::major_plan_selection`], the result of this
    /// method is *observation-only* today: nothing in the rebuild
    /// path consumes block-indexed selections yet. Once the
    /// remaining lib_test cases that depend on the legacy planner
    /// are migrated, the rebuild can switch to consume this and
    /// the legacy method goes away.
    pub(crate) fn block_plan_selection(&self, config: &OldGenConfig) -> OldGenPlanSelection {
        Self::run_major_plan_selection(self.block_region_stats(), config)
    }

    fn run_major_plan_selection(
        stats: Vec<OldRegionStats>,
        config: &OldGenConfig,
    ) -> OldGenPlanSelection {
        let mut candidates: Vec<_> = stats
            .into_iter()
            .filter(|region| {
                region.object_count > 0
                    && region.hole_bytes > 0
                    && region.hole_bytes >= config.selective_reclaim_threshold_bytes
            })
            .collect();
        candidates.sort_by(compare_compaction_candidate_priority);

        let max_regions = config.compaction_candidate_limit;
        let max_bytes = config.max_compaction_bytes_per_cycle;
        let mut selected = Vec::new();
        let mut selected_bytes = 0usize;
        for candidate in candidates {
            if selected.len() >= max_regions {
                break;
            }
            if selected_bytes.saturating_add(candidate.live_bytes) > max_bytes {
                continue;
            }
            selected_bytes = selected_bytes.saturating_add(candidate.live_bytes);
            selected.push(candidate);
        }

        OldGenPlanSelection {
            estimated_compaction_bytes: selected.iter().map(|region| region.live_bytes).sum(),
            estimated_reclaim_bytes: selected.iter().map(|region| region.hole_bytes).sum(),
            candidates: selected,
        }
    }

    pub(crate) fn prepare_rebuild(
        &mut self,
        completed_plan: Option<&CollectionPlan>,
    ) -> Option<OldRegionRebuildState> {
        if !completed_plan
            .is_some_and(|plan| matches!(plan.kind, CollectionKind::Major | CollectionKind::Full))
        {
            return None;
        }
        let previous_regions = core::mem::take(&mut self.regions);
        prepare_old_region_rebuild_for_plan(&previous_regions, completed_plan)
    }

    pub(crate) fn prepare_rebuild_for_plan(&self, plan: &CollectionPlan) -> OldRegionRebuildState {
        prepare_old_region_rebuild_for_plan(&self.regions, Some(plan))
            .expect("major reclaim preparation requires a major/full plan")
    }

    pub(crate) fn rebuild_post_sweep_object(
        config: &OldGenConfig,
        object: &mut ObjectRecord,
        total_size: usize,
        rebuild: Option<&mut OldRegionRebuildState>,
    ) {
        let Some(rebuild) = rebuild else {
            return;
        };
        let Some(placement) = object.old_region_placement() else {
            return;
        };
        let Some(placement) =
            Self::prepare_reclaim_survivor(rebuild, config, placement, total_size)
        else {
            return;
        };
        if object.old_region_placement() != Some(placement) {
            object.set_old_region_placement(placement);
        }
    }

    pub(crate) fn prepare_reclaim_survivor(
        rebuild: &mut OldRegionRebuildState,
        config: &OldGenConfig,
        mut placement: OldRegionPlacement,
        total_size: usize,
    ) -> Option<OldRegionPlacement> {
        if rebuild.selected_regions.contains(&placement.region_index) {
            let compacted =
                Self::reserve_rebuild_placement(&mut rebuild.compacted_regions, config, total_size);
            placement.region_index = rebuild.compacted_base_index + compacted.region_index;
            placement.offset_bytes = compacted.offset_bytes;
            placement.line_start = compacted.line_start;
            placement.line_count = compacted.line_count;
            let region = &mut rebuild.compacted_regions[compacted.region_index];
            region.live_bytes = region.live_bytes.saturating_add(total_size);
            region.object_count = region.object_count.saturating_add(1);
            for line in placement.line_start..placement.line_start + placement.line_count {
                region.occupied_lines.insert(line);
            }
            return Some(placement);
        }

        let &new_index = rebuild.preserved_index_map.get(&placement.region_index)?;
        placement.region_index = new_index;
        let region = &mut rebuild.rebuilt_regions[new_index];
        region.live_bytes = region.live_bytes.saturating_add(total_size);
        region.object_count = region.object_count.saturating_add(1);
        for line in placement.line_start..placement.line_start + placement.line_count {
            region.occupied_lines.insert(line);
        }
        Some(placement)
    }

    pub(crate) fn finish_rebuild(
        rebuild: Option<OldRegionRebuildState>,
        objects: &mut [ObjectRecord],
    ) -> (Option<Vec<OldRegion>>, OldRegionCollectionStats) {
        let Some(rebuild) = rebuild else {
            return (None, OldRegionCollectionStats::default());
        };
        let provisional_compacted_base = rebuild.compacted_base_index;
        let mut preserved_index_remap = vec![None; provisional_compacted_base];
        let mut compacted_regions = Vec::with_capacity(
            rebuild
                .rebuilt_regions
                .len()
                .saturating_add(rebuild.compacted_regions.len()),
        );
        for (old_index, region) in rebuild.rebuilt_regions.into_iter().enumerate() {
            if region.object_count == 0 {
                continue;
            }
            preserved_index_remap[old_index] = Some(compacted_regions.len());
            compacted_regions.push(region);
        }
        let new_compacted_base = compacted_regions.len();
        compacted_regions.extend(rebuild.compacted_regions);
        for object in objects.iter_mut() {
            if object.space() != SpaceKind::Old {
                continue;
            }
            let Some(mut placement) = object.old_region_placement() else {
                continue;
            };
            if placement.region_index < provisional_compacted_base {
                let Some(new_index) = preserved_index_remap[placement.region_index] else {
                    continue;
                };
                if placement.region_index != new_index {
                    placement.region_index = new_index;
                    object.set_old_region_placement(placement);
                }
                continue;
            }

            let compacted_offset = placement
                .region_index
                .saturating_sub(provisional_compacted_base);
            let new_index = new_compacted_base.saturating_add(compacted_offset);
            if placement.region_index != new_index {
                placement.region_index = new_index;
                object.set_old_region_placement(placement);
            }
        }
        let reclaimed_regions = rebuild
            .previous_region_count
            .saturating_sub(compacted_regions.len()) as u64;
        (
            Some(compacted_regions),
            OldRegionCollectionStats {
                compacted_regions: rebuild.compacted_regions_count,
                reclaimed_regions,
            },
        )
    }

    pub(crate) fn finish_prepared_rebuild(
        rebuild: OldRegionRebuildState,
        survivors: &mut [PreparedReclaimSurvivor],
    ) -> PreparedOldGenReclaim {
        let provisional_compacted_base = rebuild.compacted_base_index;
        let mut preserved_index_remap = vec![None; provisional_compacted_base];
        let mut compacted_regions = Vec::with_capacity(
            rebuild
                .rebuilt_regions
                .len()
                .saturating_add(rebuild.compacted_regions.len()),
        );
        for (old_index, region) in rebuild.rebuilt_regions.into_iter().enumerate() {
            if region.object_count == 0 {
                continue;
            }
            preserved_index_remap[old_index] = Some(compacted_regions.len());
            compacted_regions.push(region);
        }
        let new_compacted_base = compacted_regions.len();
        compacted_regions.extend(rebuild.compacted_regions);
        for survivor in survivors.iter_mut() {
            let Some(placement) = survivor.old_region_placement.as_mut() else {
                continue;
            };
            if placement.region_index < provisional_compacted_base {
                let Some(new_index) = preserved_index_remap[placement.region_index] else {
                    continue;
                };
                placement.region_index = new_index;
            } else {
                placement.region_index = placement
                    .region_index
                    .saturating_sub(provisional_compacted_base)
                    .saturating_add(new_compacted_base);
            }
        }
        let reclaimed_regions = rebuild
            .previous_region_count
            .saturating_sub(compacted_regions.len()) as u64;
        let reserved_bytes = compacted_regions
            .iter()
            .map(|region| region.capacity_bytes)
            .sum();
        PreparedOldGenReclaim {
            rebuilt_regions: compacted_regions,
            reserved_bytes,
            region_stats: OldRegionCollectionStats {
                compacted_regions: rebuild.compacted_regions_count,
                reclaimed_regions,
            },
        }
    }

    pub(crate) fn apply_prepared_reclaim(
        &mut self,
        prepared: PreparedOldGenReclaim,
    ) -> OldRegionCollectionStats {
        let region_stats = prepared.region_stats;
        self.regions = prepared.rebuilt_regions;
        debug_assert_eq!(self.reserved_bytes(), prepared.reserved_bytes);
        region_stats
    }

    pub(crate) fn reserve_rebuild_placement(
        regions: &mut Vec<OldRegion>,
        config: &OldGenConfig,
        bytes: usize,
    ) -> OldRegionPlacement {
        let align = config.line_bytes.max(8);

        for (region_index, region) in regions.iter_mut().enumerate() {
            let offset = align_up(region.used_bytes, align);
            if offset.saturating_add(bytes) <= region.capacity_bytes {
                region.used_bytes = offset.saturating_add(bytes);
                return Self::make_placement_for_config(config, region_index, offset, bytes);
            }
        }

        let capacity_bytes = config.region_bytes.max(bytes);
        regions.push(OldRegion {
            capacity_bytes,
            used_bytes: bytes,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
        let region_index = regions.len() - 1;
        Self::make_placement_for_config(config, region_index, 0, bytes)
    }

    fn try_reserve_in_existing_region(
        &mut self,
        bytes: usize,
        align: usize,
    ) -> Option<(usize, usize)> {
        for (region_index, region) in self.regions.iter_mut().enumerate() {
            let offset = align_up(region.used_bytes, align);
            if offset.saturating_add(bytes) <= region.capacity_bytes {
                region.used_bytes = offset.saturating_add(bytes);
                return Some((region_index, offset));
            }
        }
        None
    }

    fn make_placement(
        &self,
        config: &OldGenConfig,
        region_index: usize,
        offset_bytes: usize,
        bytes: usize,
    ) -> OldRegionPlacement {
        Self::make_placement_for_config(config, region_index, offset_bytes, bytes)
    }

    fn make_placement_for_config(
        config: &OldGenConfig,
        region_index: usize,
        offset_bytes: usize,
        bytes: usize,
    ) -> OldRegionPlacement {
        let line_bytes = config.line_bytes.max(1);
        let line_start = offset_bytes / line_bytes;
        let line_count = bytes.div_ceil(line_bytes).max(1);
        OldRegionPlacement {
            region_index,
            offset_bytes,
            line_start,
            line_count,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OldRegion {
    pub(crate) capacity_bytes: usize,
    pub(crate) used_bytes: usize,
    pub(crate) live_bytes: usize,
    pub(crate) object_count: usize,
    pub(crate) occupied_lines: HashSet<usize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OldRegionCollectionStats {
    pub(crate) compacted_regions: u64,
    pub(crate) reclaimed_regions: u64,
}

#[derive(Debug)]
pub(crate) struct OldRegionRebuildState {
    pub(crate) previous_region_count: usize,
    pub(crate) preserved_index_map: HashMap<usize, usize>,
    pub(crate) selected_regions: HashSet<usize>,
    pub(crate) compacted_base_index: usize,
    pub(crate) compacted_regions_count: u64,
    pub(crate) rebuilt_regions: Vec<OldRegion>,
    pub(crate) compacted_regions: Vec<OldRegion>,
}

pub(crate) fn prepare_old_region_rebuild_for_plan(
    previous_regions: &[OldRegion],
    completed_plan: Option<&CollectionPlan>,
) -> Option<OldRegionRebuildState> {
    let plan = completed_plan
        .filter(|plan| matches!(plan.kind, CollectionKind::Major | CollectionKind::Full))?;
    let previous_region_count = previous_regions.len();
    let selected_regions: HashSet<_> = plan.selected_old_regions.iter().copied().collect();
    let mut rebuilt_regions = Vec::new();
    let mut preserved_index_map = HashMap::new();
    for (old_index, region) in previous_regions.iter().enumerate() {
        if selected_regions.contains(&old_index) {
            continue;
        }
        preserved_index_map.insert(old_index, rebuilt_regions.len());
        rebuilt_regions.push(OldRegion {
            capacity_bytes: region.capacity_bytes,
            used_bytes: region.used_bytes,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
    }
    let compacted_base_index = rebuilt_regions.len();
    Some(OldRegionRebuildState {
        previous_region_count,
        preserved_index_map,
        selected_regions,
        compacted_base_index,
        compacted_regions_count: plan.selected_old_regions.len() as u64,
        rebuilt_regions,
        compacted_regions: Vec::new(),
    })
}

pub(crate) fn compare_compaction_candidate_priority(
    left: &OldRegionStats,
    right: &OldRegionStats,
) -> core::cmp::Ordering {
    let left_live = left.live_bytes.max(1) as u128;
    let right_live = right.live_bytes.max(1) as u128;
    let left_efficiency = (left.hole_bytes as u128).saturating_mul(right_live);
    let right_efficiency = (right.hole_bytes as u128).saturating_mul(left_live);

    right_efficiency
        .cmp(&left_efficiency)
        .then_with(|| right.hole_bytes.cmp(&left.hole_bytes))
        .then_with(|| left.live_bytes.cmp(&right.live_bytes))
        .then_with(|| right.free_bytes.cmp(&left.free_bytes))
        .then_with(|| left.object_count.cmp(&right.object_count))
        .then_with(|| left.region_index.cmp(&right.region_index))
}

fn align_up(value: usize, align: usize) -> usize {
    if align <= 1 {
        value
    } else {
        let rem = value % align;
        if rem == 0 {
            value
        } else {
            value + (align - rem)
        }
    }
}

#[cfg(test)]
#[path = "old_test.rs"]
mod tests;
