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
    /// Concurrent mark wall-clock duration for this cycle.
    ///
    /// For a major/full cycle this is measured from
    /// `begin_major_mark` to the moment the active session is
    /// finished or its reclaim is prepared. For a minor cycle
    /// this is zero (minor cycles do not run a concurrent mark
    /// session). The cumulative `HeapStats.collections.mark_nanos`
    /// counter is the sum across every completed major/full
    /// cycle and corresponds to the "concurrent mark duration"
    /// telemetry surface required by `DESIGN.md`.
    pub mark_nanos: u64,
    /// Time spent preparing reclaim state ahead of the final commit for this cycle.
    pub reclaim_prepare_nanos: u64,
    /// Bytes promoted from nursery to old generation.
    pub promoted_bytes: u64,
    /// Bytes that were live in the nursery immediately before
    /// this cycle's evacuation phase began.
    ///
    /// Populated by minor cycles only. Major cycles do not run a
    /// nursery evacuation pass at this layer and report zero.
    /// Together with [`nursery_survivor_bytes`](Self::nursery_survivor_bytes)
    /// this gives the raw inputs callers need to compute a
    /// nursery survival rate without losing precision to a fixed
    /// floating-point form. The cumulative
    /// `HeapStats.collections.nursery_bytes_before` counter is
    /// the sum across every completed minor cycle and corresponds
    /// to the "nursery survival rate" telemetry surface required
    /// by `DESIGN.md`.
    pub nursery_bytes_before: u64,
    /// Bytes that survived the cycle's nursery evacuation, summed
    /// across both the bytes that aged into the next semispace
    /// and the bytes that were promoted out to the old generation
    /// (or another non-nursery space).
    ///
    /// Populated by minor cycles only. The lifetime cumulative
    /// counter on [`HeapStats::collections`] gives total nursery
    /// survivors across the heap's life; consumers can divide it
    /// by [`nursery_bytes_before`](Self::nursery_bytes_before) to
    /// compute a long-term survival ratio.
    pub nursery_survivor_bytes: u64,
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
        nursery_bytes_before: usize,
        nursery_bytes_after: usize,
        before_bytes: usize,
        after_bytes: usize,
        queued_finalizers: u64,
        old_region_stats: OldRegionCollectionStats,
    ) -> Self {
        // Survivors include the bytes that aged into the next
        // semispace plus the bytes promoted out to old/pinned.
        let nursery_survivor_bytes = (nursery_bytes_after as u64)
            .saturating_add(promoted_bytes as u64);
        Self {
            collections: 1,
            minor_collections: 1,
            major_collections: 0,
            pause_nanos: 0,
            mark_nanos: 0,
            reclaim_prepare_nanos: 0,
            promoted_bytes: promoted_bytes as u64,
            nursery_bytes_before: nursery_bytes_before as u64,
            nursery_survivor_bytes,
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
        mark_elapsed_nanos: u64,
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
            mark_nanos: mark_elapsed_nanos,
            reclaim_prepare_nanos,
            promoted_bytes: promoted_bytes as u64,
            nursery_bytes_before: 0,
            nursery_survivor_bytes: 0,
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
        self.mark_nanos = self.mark_nanos.saturating_add(other.mark_nanos);
        self.reclaim_prepare_nanos = self
            .reclaim_prepare_nanos
            .saturating_add(other.reclaim_prepare_nanos);
        self.promoted_bytes = self.promoted_bytes.saturating_add(other.promoted_bytes);
        self.nursery_bytes_before = self
            .nursery_bytes_before
            .saturating_add(other.nursery_bytes_before);
        self.nursery_survivor_bytes = self
            .nursery_survivor_bytes
            .saturating_add(other.nursery_survivor_bytes);
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

/// Cumulative write-barrier traffic counters.
///
/// The DESIGN.md telemetry contract calls out "barrier traffic"
/// as a required observability surface. These counters are bumped
/// every time the runtime pushes a [`crate::barrier::BarrierEvent`]
/// for a mutator-side write, broken down by
/// [`crate::barrier::BarrierKind`]:
///
/// * [`post_write`](Self::post_write) — every post-write barrier
///   call regardless of whether the slot landed in the remembered
///   set. Counts pure mutation traffic.
/// * [`satb_pre_write`](Self::satb_pre_write) — only the post-
///   write barriers that also fired the SATB pre-write hook
///   because a major mark session was active and the overwritten
///   slot held a managed reference. This is the metric to watch
///   when reasoning about marker overhead during incremental
///   cycles.
///
/// Counters are monotonic for the lifetime of one [`crate::Heap`]
/// (and one [`crate::SharedHeap`] backing it). Diff two
/// snapshots to attribute work to a particular interval.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BarrierStats {
    /// Number of post-write barriers recorded across the heap's
    /// lifetime. Bumped once per
    /// [`crate::barrier::BarrierKind::PostWrite`] event.
    pub post_write: u64,
    /// Number of SATB pre-write barriers recorded across the
    /// heap's lifetime. Bumped once per
    /// [`crate::barrier::BarrierKind::SatbPreWrite`] event, which
    /// only fires when a major mark session is active and the
    /// overwritten slot carried a managed reference.
    pub satb_pre_write: u64,
}

/// Cumulative physical old-gen compaction counters.
///
/// Populated by [`crate::heap::Heap::compact_old_gen_physical`]
/// (and the mutator + shared-heap wrappers). Counters are
/// monotonic: they only grow. Users can diff two snapshots to
/// attribute work to a particular interval.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompactionStats {
    /// Total number of `compact_old_gen_physical` calls that ran
    /// and actually moved at least one record.
    pub cycles: u64,
    /// Total number of records physically evacuated across every
    /// compaction call.
    pub records_moved: u64,
    /// Total number of freshly-created target blocks the
    /// compaction pass allocated to hold evacuated records. With
    /// the pack-targets rewrite a single target block can host
    /// many survivors, so this is typically much smaller than
    /// `records_moved`.
    pub target_blocks_created: u64,
    /// Total number of source blocks reclaimed by the post-
    /// compact rebuild pass because no surviving record still
    /// points into them.
    pub source_blocks_reclaimed: u64,
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
    ///
    /// This is the unified view: the sum of the explicit-edge
    /// fallback path ([`Self::remembered_explicit_edges`]) and
    /// the per-block dirty-card fast path
    /// ([`Self::remembered_dirty_cards`]). Readers that need to
    /// attribute remembered-set pressure to one path or the
    /// other should consult the split counters below.
    pub remembered_edges: usize,
    /// Number of distinct old owners represented in the remembered set.
    pub remembered_owners: usize,
    /// Number of remembered edges recorded via the legacy
    /// explicit-edge fallback path.
    ///
    /// This path fires when the owner of a post-write barrier
    /// is not backed by an old-gen block (pinned space, large
    /// object space, or a system-allocated old-gen survivor
    /// that could not fit in a block hole). Each entry is a
    /// full `(owner, target)` pair stored in a dense `Vec`, so
    /// this counter is a rough proxy for fallback-path memory
    /// pressure.
    ///
    /// In the DESIGN.md final-goal target, every old-gen byte
    /// lives in a block-backed region with its own card table,
    /// so this counter should drift toward zero as pinned and
    /// large spaces migrate to the block model. Today it is
    /// non-zero for workloads that allocate pinned or large
    /// objects and mutate their contents to point at nursery
    /// survivors.
    pub remembered_explicit_edges: usize,
    /// Number of dirty cards currently marked across the old-
    /// gen block pool.
    ///
    /// Each dirty card represents at least one pending
    /// old-to-young root in its covered byte range. The minor
    /// GC's dirty-card scan walks these cards to find the
    /// records living in them and adds those records as
    /// additional trace sources.
    ///
    /// Dirty cards are the fast-path write barrier: each
    /// barrier is an O(1) card byte store, and the minor GC
    /// scans O(dirty_cards) rather than O(recorded edges).
    pub remembered_dirty_cards: usize,
    /// Total bytes the old-generation block allocator has bumped
    /// past across every block in the pool. This is the sum of
    /// `block.used_bytes()` over every block, where `used_bytes`
    /// is the byte offset the bump allocator has advanced to
    /// inside that block (including any interior holes left by
    /// dead objects).
    ///
    /// Unlike [`SpaceStats::live_bytes`], `old_gen_used_bytes`
    /// also covers the "hole bytes" that sit between surviving
    /// objects inside used lines. The difference between this
    /// counter and `old.live_bytes` is exactly the old-gen
    /// fragmentation that drives the physical-compaction
    /// decision: `holes = old_gen_used_bytes - old.live_bytes`.
    ///
    /// Cached into the shared snapshot so
    /// [`crate::SharedHeap::old_gen_fragmentation_ratio`] can
    /// reconstruct the ratio lock-free instead of walking the
    /// block pool under the heap read lock.
    pub old_gen_used_bytes: usize,
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

    pub(crate) fn record_allocation(
        &mut self,
        space: SpaceKind,
        bytes: usize,
        old_reserved_bytes: usize,
    ) {
        match space {
            SpaceKind::Nursery => {
                self.nursery.live_bytes = self.nursery.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Old => {
                self.old.live_bytes = self.old.live_bytes.saturating_add(bytes);
                self.old.reserved_bytes = old_reserved_bytes;
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

    pub(crate) fn apply_space_rebuild(
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
