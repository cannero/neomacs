use crate::object::SpaceKind;
use crate::spaces::OldRegionCollectionStats;

/// Collection statistics for one completed GC cycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionStats {
    /// Number of collections that have completed.
    pub collections: u64,
    /// Number of nursery collections.
    pub minor_collections: u64,
    /// Number of old-generation collections.
    pub major_collections: u64,
    /// Stop-the-world time spent inside the call that completed this cycle.
    pub pause_nanos: u64,
    /// Time spent preparing reclaim state ahead of the final commit for this cycle.
    pub reclaim_prepare_nanos: u64,
    /// Bytes promoted from nursery to old generation.
    pub promoted_bytes: u64,
    /// Number of mark slices drained across completed GC cycles.
    pub mark_steps: u64,
    /// Number of mark worker rounds drained across completed GC cycles.
    pub mark_rounds: u64,
    /// Bytes reclaimed across completed GC cycles.
    pub reclaimed_bytes: u64,
    /// Number of finalizers run synchronously during completed GC cycles.
    pub finalized_objects: u64,
    /// Number of dead finalizable objects queued for later draining across completed GC cycles.
    pub queued_finalizers: u64,
    /// Number of old-generation regions compacted across completed GC cycles.
    pub compacted_regions: u64,
    /// Number of old-generation regions reclaimed across completed GC cycles.
    pub reclaimed_regions: u64,
}

impl CollectionStats {
    pub(crate) fn completed_minor_cycle(
        mark_steps: u64,
        mark_rounds: u64,
        promoted_bytes: usize,
        before_bytes: usize,
        after_bytes: usize,
        queued_finalizers: u64,
        old_region_stats: OldRegionCollectionStats,
    ) -> Self {
        Self {
            collections: 1,
            minor_collections: 1,
            major_collections: 0,
            pause_nanos: 0,
            reclaim_prepare_nanos: 0,
            promoted_bytes: promoted_bytes as u64,
            mark_steps,
            mark_rounds,
            reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
            finalized_objects: 0,
            queued_finalizers,
            compacted_regions: old_region_stats.compacted_regions,
            reclaimed_regions: old_region_stats.reclaimed_regions,
        }
    }

    pub(crate) fn completed_old_gen_cycle(
        mark_steps: u64,
        mark_rounds: u64,
        promoted_bytes: usize,
        reclaim_prepare_nanos: u64,
        before_bytes: usize,
        after_bytes: usize,
        queued_finalizers: u64,
        old_region_stats: OldRegionCollectionStats,
    ) -> Self {
        Self {
            collections: 1,
            minor_collections: 0,
            major_collections: 1,
            pause_nanos: 0,
            reclaim_prepare_nanos,
            promoted_bytes: promoted_bytes as u64,
            mark_steps,
            mark_rounds,
            reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
            finalized_objects: 0,
            queued_finalizers,
            compacted_regions: old_region_stats.compacted_regions,
            reclaimed_regions: old_region_stats.reclaimed_regions,
        }
    }

    pub(crate) fn saturating_add_assign(&mut self, other: CollectionStats) {
        self.collections = self.collections.saturating_add(other.collections);
        self.minor_collections = self
            .minor_collections
            .saturating_add(other.minor_collections);
        self.major_collections = self
            .major_collections
            .saturating_add(other.major_collections);
        self.pause_nanos = self.pause_nanos.saturating_add(other.pause_nanos);
        self.reclaim_prepare_nanos = self
            .reclaim_prepare_nanos
            .saturating_add(other.reclaim_prepare_nanos);
        self.promoted_bytes = self.promoted_bytes.saturating_add(other.promoted_bytes);
        self.mark_steps = self.mark_steps.saturating_add(other.mark_steps);
        self.mark_rounds = self.mark_rounds.saturating_add(other.mark_rounds);
        self.reclaimed_bytes = self.reclaimed_bytes.saturating_add(other.reclaimed_bytes);
        self.finalized_objects = self
            .finalized_objects
            .saturating_add(other.finalized_objects);
        self.queued_finalizers = self
            .queued_finalizers
            .saturating_add(other.queued_finalizers);
        self.compacted_regions = self
            .compacted_regions
            .saturating_add(other.compacted_regions);
        self.reclaimed_regions = self
            .reclaimed_regions
            .saturating_add(other.reclaimed_regions);
    }
}

/// Per-space storage statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpaceStats {
    /// Bytes reserved by the space.
    pub reserved_bytes: usize,
    /// Bytes currently live in the space.
    pub live_bytes: usize,
}

/// Public snapshot of one logical old-generation region.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OldRegionStats {
    /// Region index in allocation order.
    pub region_index: usize,
    /// Bytes reserved for this region.
    pub reserved_bytes: usize,
    /// Bytes currently consumed by the region allocation cursor.
    pub used_bytes: usize,
    /// Bytes currently live in this region.
    pub live_bytes: usize,
    /// Reclaimable bytes in this region.
    pub free_bytes: usize,
    /// Bytes lost to interior holes between live objects.
    pub hole_bytes: usize,
    /// Unused bytes still available at the end of the region.
    pub tail_bytes: usize,
    /// Number of live objects assigned to this region.
    pub object_count: usize,
    /// Number of occupied lines containing live objects.
    pub occupied_lines: usize,
}

/// Heap-wide runtime statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeapStats {
    /// Nursery statistics.
    pub nursery: SpaceStats,
    /// Old-generation statistics.
    pub old: SpaceStats,
    /// Pinned-space statistics.
    pub pinned: SpaceStats,
    /// Large-object-space statistics.
    pub large: SpaceStats,
    /// Immortal-space statistics.
    pub immortal: SpaceStats,
    /// Collection counters.
    pub collections: CollectionStats,
    /// Number of remembered old-to-young edges currently tracked.
    pub remembered_edges: usize,
    /// Number of distinct old owners represented in the remembered set.
    pub remembered_owners: usize,
    /// Number of finalizable objects currently tracked as reclaim candidates.
    pub finalizable_candidates: usize,
    /// Number of weak-bearing objects currently tracked as reclaim candidates.
    pub weak_candidates: usize,
    /// Number of ephemeron-bearing objects currently tracked as reclaim candidates.
    pub ephemeron_candidates: usize,
    /// Number of queued finalizers that have run through explicit drain calls.
    pub finalizers_run: u64,
    /// Number of queued finalizers that are waiting to run.
    pub pending_finalizers: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PreparedHeapStats {
    pub(crate) nursery: SpaceStats,
    pub(crate) old: SpaceStats,
    pub(crate) pinned: SpaceStats,
    pub(crate) large: SpaceStats,
    pub(crate) immortal: SpaceStats,
}

impl HeapStats {
    pub(crate) fn total_live_bytes(&self) -> usize {
        self.nursery
            .live_bytes
            .saturating_add(self.old.live_bytes)
            .saturating_add(self.pinned.live_bytes)
            .saturating_add(self.large.live_bytes)
            .saturating_add(self.immortal.live_bytes)
    }
}

impl PreparedHeapStats {
    pub(crate) fn record_live_object(&mut self, space: SpaceKind, bytes: usize) {
        match space {
            SpaceKind::Nursery => {
                self.nursery.live_bytes = self.nursery.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Old => {
                self.old.live_bytes = self.old.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Pinned => {
                self.pinned.live_bytes = self.pinned.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Large => {
                self.large.live_bytes = self.large.live_bytes.saturating_add(bytes);
                self.large.reserved_bytes = self.large.reserved_bytes.saturating_add(bytes);
            }
            SpaceKind::Immortal => {
                self.immortal.live_bytes = self.immortal.live_bytes.saturating_add(bytes);
                self.immortal.reserved_bytes = self.immortal.reserved_bytes.saturating_add(bytes);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn total_live_bytes(&self) -> usize {
        self.nursery
            .live_bytes
            .saturating_add(self.old.live_bytes)
            .saturating_add(self.pinned.live_bytes)
            .saturating_add(self.large.live_bytes)
            .saturating_add(self.immortal.live_bytes)
    }

    pub(crate) fn apply_prepared_reclaim(
        self,
        stats: &mut HeapStats,
        old_reserved_bytes: usize,
    ) -> usize {
        stats.nursery.live_bytes = self.nursery.live_bytes;
        stats.old.live_bytes = self.old.live_bytes;
        stats.old.reserved_bytes = old_reserved_bytes;
        stats.pinned.live_bytes = self.pinned.live_bytes;
        stats.large.live_bytes = self.large.live_bytes;
        stats.large.reserved_bytes = self.large.reserved_bytes;
        stats.immortal.live_bytes = self.immortal.live_bytes;
        stats.immortal.reserved_bytes = self.immortal.reserved_bytes;
        stats.total_live_bytes()
    }
}

#[cfg(test)]
#[path = "stats_test.rs"]
mod tests;
