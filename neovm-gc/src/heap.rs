use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::collector_exec::collect_global_sources;
use crate::collector_state::{CollectorSharedSnapshot, CollectorStateHandle};
use crate::descriptor::{GcErased, Trace, TypeDesc, fixed_type_desc};
use crate::index_state::HeapIndexState;
use crate::mutator::Mutator;
use crate::object::{ObjectRecord, SpaceKind};
use crate::pacer::{Pacer, PacerConfig, PacerStats};
use crate::pause_stats::{PauseHistogram, PauseStatsHandle};
use crate::plan::{
    CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress, RuntimeWorkStatus,
};
use crate::root::RootStack;
use crate::runtime::CollectorRuntime;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, NurseryState, OldGenConfig, OldGenPlanSelection,
    OldGenState, PinnedSpaceConfig,
};
use crate::stats::{CollectionStats, HeapStats, OldRegionStats};
use core::any::TypeId;
use core::ptr::NonNull;
use std::collections::HashMap;

/// Heap creation configuration.
///
/// `Eq` is intentionally not derived because the embedded
/// `PacerConfig` carries `f64` fields (allocation rates, ratios).
/// `PartialEq` is still implemented so callers can compare configs
/// for testing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeapConfig {
    /// Nursery configuration.
    pub nursery: NurseryConfig,
    /// Old-generation configuration.
    pub old: OldGenConfig,
    /// Pinned-space configuration.
    pub pinned: PinnedSpaceConfig,
    /// Large-object-space configuration.
    pub large: LargeObjectSpaceConfig,
    /// Adaptive pacer configuration. Defaults to
    /// [`PacerConfig::default`], which keeps the pacer enabled with
    /// conservative trigger thresholds. The pacer can also be
    /// reconfigured after construction via [`Heap::set_pacer_config`].
    pub pacer: PacerConfig,
}

/// Allocation error for the managed heap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocError {
    /// Object size overflowed layout computation.
    LayoutOverflow,
    /// Allocator returned null for the requested size.
    OutOfMemory {
        /// Total allocation size requested from the system allocator.
        requested_bytes: usize,
    },
    /// A persistent major collection session is already active.
    CollectionInProgress,
    /// No persistent major collection session is currently active.
    NoCollectionInProgress,
    /// The requested collection kind is not supported for this API.
    UnsupportedCollectionKind {
        /// Collection kind that could not be honored.
        kind: CollectionKind,
    },
}

/// Global heap object.
///
/// Field order matters: `ObjectRecord` storage (both the live `objects`
/// vec and the `runtime_state` pending-finalizer queue) must drop
/// BEFORE the `NurseryState` that backs their arena allocations,
/// otherwise arena-owned records would run their payload `Drop`
/// implementations against already-freed buffers.
#[derive(Debug)]
pub struct Heap {
    config: HeapConfig,
    stats: HeapStats,
    roots: RootStack,
    descriptors: HashMap<TypeId, &'static TypeDesc>,
    // --- record storage (drops first, before nursery) ---
    objects: Vec<ObjectRecord>,
    runtime_state: RuntimeStateHandle,
    // --- index and collector bookkeeping ---
    indexes: HeapIndexState,
    old_gen: OldGenState,
    recent_barrier_events: Vec<BarrierEvent>,
    collector: CollectorStateHandle,
    pause_stats: PauseStatsHandle,
    pacer: Pacer,
    /// Cumulative physical old-gen compaction counters. Updated
    /// by `compact_old_gen_physical` after every call that
    /// actually moves at least one record.
    compaction_stats: crate::stats::CompactionStats,
    /// Cumulative write-barrier traffic counters. Updated by
    /// `push_barrier_event` for every barrier kind the runtime
    /// records.
    barrier_stats: crate::stats::BarrierStats,
    // --- arena buffers (drops last, after all records) ---
    /// Bump-pointer semispace nursery arenas (Phase 1).
    nursery: NurseryState,
}

// SAFETY: `Heap` owns all heap allocations and its raw pointers are internal references into that
// owned storage or static descriptors. Sending a `Heap` to another thread does not invalidate those
// pointers. Concurrent access is still not allowed without external synchronization, so `Heap` is
// `Send` but intentionally not `Sync`.
unsafe impl Send for Heap {}

impl Heap {
    /// Create a new heap with `config`.
    pub fn new(config: HeapConfig) -> Self {
        let nursery = NurseryState::new(config.nursery.semispace_bytes);
        let pacer = Pacer::new(config.pacer);
        let heap = Self {
            stats: HeapStats {
                nursery: crate::stats::SpaceStats {
                    reserved_bytes: config.nursery.semispace_bytes.saturating_mul(2),
                    live_bytes: 0,
                },
                old: crate::stats::SpaceStats {
                    reserved_bytes: 0,
                    live_bytes: 0,
                },
                pinned: crate::stats::SpaceStats {
                    reserved_bytes: config.pinned.reserved_bytes,
                    live_bytes: 0,
                },
                large: crate::stats::SpaceStats::default(),
                immortal: crate::stats::SpaceStats::default(),
                collections: crate::stats::CollectionStats::default(),
                remembered_edges: 0,
                remembered_owners: 0,
                finalizable_candidates: 0,
                weak_candidates: 0,
                ephemeron_candidates: 0,
                finalizers_run: 0,
                pending_finalizers: 0,
            },
            config,
            roots: RootStack::default(),
            descriptors: HashMap::default(),
            objects: Vec::new(),
            indexes: HeapIndexState::default(),
            old_gen: OldGenState::default(),
            runtime_state: RuntimeStateHandle::default(),
            recent_barrier_events: Vec::new(),
            collector: CollectorStateHandle::default(),
            pause_stats: PauseStatsHandle::new(),
            pacer,
            compaction_stats: crate::stats::CompactionStats::default(),
            barrier_stats: crate::stats::BarrierStats::default(),
            nursery,
        };
        heap.refresh_recommended_plans();
        heap
    }

    #[allow(dead_code)]
    pub(crate) fn nursery(&self) -> &NurseryState {
        &self.nursery
    }

    pub(crate) fn nursery_mut(&mut self) -> &mut NurseryState {
        &mut self.nursery
    }

    pub(crate) fn runtime_state_handle(&self) -> RuntimeStateHandle {
        self.runtime_state.clone()
    }

    pub(crate) fn collector_handle(&self) -> CollectorStateHandle {
        self.collector.clone()
    }

    pub(crate) fn global_sources(&self) -> Vec<GcErased> {
        collect_global_sources(&self.roots, &self.objects)
    }

    pub(crate) fn objects(&self) -> &[ObjectRecord] {
        &self.objects
    }

    pub(crate) fn indexes(&self) -> &HeapIndexState {
        &self.indexes
    }

    pub(crate) fn old_gen(&self) -> &OldGenState {
        &self.old_gen
    }

    pub(crate) fn old_gen_mut(&mut self) -> &mut OldGenState {
        &mut self.old_gen
    }

    pub(crate) fn old_config(&self) -> &OldGenConfig {
        &self.config.old
    }

    /// Physical old-gen compaction (opt-in, stop-the-world).
    ///
    /// Evacuates every surviving record in any `OldBlock` whose
    /// live density is at or below `density_threshold` into
    /// freshly-created target blocks, rewrites every inbound
    /// reference via the existing forwarding relocator, and
    /// reclaims the now-empty source blocks.
    ///
    /// Unlike the logical "region" compaction that runs inside a
    /// major cycle, this actually moves bytes: the source block's
    /// payload storage is abandoned and the survivors live in
    /// fresh target blocks. After the call,
    /// `block.used_bytes() - block.live_bytes()` on the fresh
    /// target blocks reflects the tight packed layout, so metrics
    /// that measure "hole bytes" (e.g. `OldRegionStats::hole_bytes`)
    /// genuinely shrink.
    ///
    /// `density_threshold` is in `[0.0, 1.0]`. At 0.0 the threshold
    /// never fires (nothing is compacted). At 1.0 every block with
    /// any empty space becomes a candidate.
    ///
    /// Returns the number of records physically evacuated.
    ///
    /// This method assumes the caller has just completed a mark
    /// phase that identified every live record. It does NOT run a
    /// mark pass itself: dead records must already be gone from
    /// `objects`, or the compaction will waste effort moving them.
    /// In practice callers should invoke this right after a major
    /// cycle to get physical compaction of the post-mark heap.
    ///
    /// # Typical usage
    ///
    /// Two common patterns:
    ///
    /// 1. **Automatic invocation**: set
    ///    `OldGenConfig::physical_compaction_density_threshold`
    ///    above 0.0 in `HeapConfig` and the runtime hooks in
    ///    `execute_plan` and `commit_finished_active_collection`
    ///    will call `compact_old_gen_physical` after every major
    ///    cycle. Most callers want this.
    ///
    /// 2. **Manual invocation**: keep the threshold at 0.0 in
    ///    config and call `mutator.compact_old_gen_physical(...)`
    ///    explicitly at chosen safepoints. Useful for callers
    ///    that want to time compaction against their own
    ///    workload pattern (idle periods, post-checkpoint, etc.).
    ///
    /// For the conditional variant that only runs compaction
    /// when fragmentation actually warrants it, see
    /// [`Heap::compact_old_gen_if_fragmented`]. For multi-pass
    /// bulk cleanup before a long idle period see
    /// [`Heap::compact_old_gen_aggressive`]. For a cheap
    /// "should I bother?" predicate see
    /// [`Heap::should_compact_old_gen`].
    ///
    /// # Example
    ///
    /// ```
    /// use neovm_gc::{Heap, HeapConfig};
    ///
    /// // A fresh heap has no old-gen records, so compaction
    /// // at any threshold is a no-op.
    /// let mut heap = Heap::new(HeapConfig::default());
    /// let moved = heap.compact_old_gen_physical(0.5);
    /// assert_eq!(moved, 0);
    /// assert_eq!(heap.compaction_stats().cycles, 0);
    /// ```
    pub fn compact_old_gen_physical(&mut self, density_threshold: f64) -> usize {
        let runtime_state = self.runtime_state.clone();
        let block_count_before = self.old_gen.block_count();
        let Self {
            roots,
            objects,
            indexes,
            old_gen,
            config,
            ..
        } = self;
        let old_config = &config.old;
        let forwarding = crate::reclaim::compact_sparse_old_blocks(
            objects,
            old_gen,
            old_config,
            density_threshold,
        );
        let moved = forwarding.len();
        if moved == 0 {
            return 0;
        }
        crate::spaces::nursery::relocate_roots_and_edges(roots, objects, indexes, &forwarding);
        let block_count_after_evacuation = old_gen.block_count();
        // After the compaction pass: source blocks have stale
        // line_marks reflecting their pre-compaction placements,
        // and fresh target blocks have zeroed line_marks because
        // their allocations did not go through the sweep path.
        // Rebuild line marks across every surviving block-backed
        // record so the source blocks (now with no live records)
        // become empty and get dropped, and the fresh targets get
        // their line_marks repopulated from the survivors that
        // now live in them.
        crate::reclaim::rebuild_line_marks_and_reclaim_empty_old_blocks(
            objects,
            old_gen,
            &runtime_state,
        );
        let block_count_after_rebuild = old_gen.block_count();
        // target_blocks_created = blocks that appeared between
        // the pre-compact count and the post-evacuation count.
        // source_blocks_reclaimed = blocks that disappeared
        // between the post-evacuation count and the post-rebuild
        // count.
        let target_blocks_created =
            block_count_after_evacuation.saturating_sub(block_count_before) as u64;
        let source_blocks_reclaimed = block_count_after_evacuation
            .saturating_sub(block_count_after_rebuild) as u64;
        self.compaction_stats.cycles = self.compaction_stats.cycles.saturating_add(1);
        self.compaction_stats.records_moved = self
            .compaction_stats
            .records_moved
            .saturating_add(moved as u64);
        self.compaction_stats.target_blocks_created = self
            .compaction_stats
            .target_blocks_created
            .saturating_add(target_blocks_created);
        self.compaction_stats.source_blocks_reclaimed = self
            .compaction_stats
            .source_blocks_reclaimed
            .saturating_add(source_blocks_reclaimed);
        moved
    }

    /// Cumulative physical compaction counters since heap
    /// construction. See [`crate::stats::CompactionStats`].
    pub fn compaction_stats(&self) -> crate::stats::CompactionStats {
        self.compaction_stats
    }

    /// Reset every counter in [`Heap::compaction_stats`] to
    /// zero. Useful for callers that want to measure compaction
    /// work over a specific interval rather than over the
    /// entire heap lifetime: snapshot, do work, read again, no
    /// arithmetic needed.
    pub fn clear_compaction_stats(&mut self) {
        self.compaction_stats = crate::stats::CompactionStats::default();
    }

    /// Current nursery fill ratio: `live_bytes / capacity` of
    /// the from-space arena. Returns `0.0` when the from-space
    /// is empty or the capacity is zero. Range `[0.0, 1.0]`:
    /// 0.0 means the nursery is empty, 1.0 means it is full.
    ///
    /// Useful for callers that want to decide when to trigger
    /// a minor cycle without waiting for the static nursery
    /// pressure plan to fire (the same job the pacer's nursery
    /// soft trigger does, but exposed as a raw value rather
    /// than a decision).
    pub fn nursery_fill_ratio(&self) -> f64 {
        let capacity = self.nursery.capacity();
        if capacity == 0 {
            return 0.0;
        }
        let used = self.nursery.from_space().used_bytes();
        (used as f64) / (capacity as f64)
    }

    /// Current old-gen fragmentation ratio computed from the
    /// block-side counters. Defined as
    /// `total_hole_bytes / max(total_used_bytes, 1)` where
    /// `total_hole_bytes = sum(block.used_bytes - block.live_bytes)`
    /// across every block. Returns `0.0` when the pool is empty.
    /// Range `[0.0, 1.0]`: 0.0 means every block is packed
    /// tight, 1.0 means every block is entirely wasted space.
    ///
    /// Reading this is cheap (one linear scan over the block
    /// pool) and safe to call whenever the heap is accessible
    /// via its owned reference.
    pub fn old_gen_fragmentation_ratio(&self) -> f64 {
        let blocks = self.old_gen.blocks();
        if blocks.is_empty() {
            return 0.0;
        }
        let mut total_used = 0usize;
        let mut total_live = 0usize;
        for block in blocks {
            total_used = total_used.saturating_add(block.used_bytes());
            total_live = total_live.saturating_add(block.live_bytes());
        }
        if total_used == 0 {
            return 0.0;
        }
        let holes = total_used.saturating_sub(total_live);
        (holes as f64) / (total_used as f64)
    }

    /// Opportunistic physical compaction: compute the current
    /// old-gen fragmentation ratio and, if it exceeds
    /// `fragmentation_threshold`, run
    /// [`Heap::compact_old_gen_physical`] at the configured
    /// density threshold (or a permissive 0.5 fallback if the
    /// config is set to the default 0.0 disabled value).
    ///
    /// Returns `(fragmentation, records_moved)` so callers can
    /// distinguish "no compaction run" (moved == 0) from "compaction
    /// ran but nothing qualified" (fragmentation met threshold but
    /// no sparse blocks).
    pub fn compact_old_gen_if_fragmented(
        &mut self,
        fragmentation_threshold: f64,
    ) -> (f64, usize) {
        let frag = self.old_gen_fragmentation_ratio();
        if frag < fragmentation_threshold {
            return (frag, 0);
        }
        let density = self
            .config
            .old
            .physical_compaction_density_threshold
            .max(0.5);
        let moved = self.compact_old_gen_physical(density);
        (frag, moved)
    }

    /// Predicate-only version of [`Heap::compact_old_gen_if_fragmented`]:
    /// returns `true` when the current old-gen fragmentation
    /// ratio is at or above `fragmentation_threshold` AND at
    /// least one block exists in the pool. Callers (schedulers,
    /// pacers, background workers) can use this as a cheap
    /// "should I run compaction now?" check before grabbing the
    /// heap lock for an actual compact call.
    ///
    /// The check is read-only on the heap state and never
    /// allocates.
    pub fn should_compact_old_gen(&self, fragmentation_threshold: f64) -> bool {
        if self.old_gen.block_count() == 0 {
            return false;
        }
        self.old_gen_fragmentation_ratio() >= fragmentation_threshold
    }

    /// Run [`Heap::compact_old_gen_physical`] in a loop at the
    /// supplied density threshold until either no more progress
    /// is made or the loop has run `max_passes` times. Returns
    /// the total number of records evacuated across every pass.
    ///
    /// Convergence is detected by tracking the block count
    /// BEFORE each pass. Compaction ALWAYS creates at least one
    /// fresh target block when it moves any record, so a pass
    /// is "productive" only when the post-compact block count
    /// is strictly LESS than the pre-compact count (i.e. more
    /// source blocks were dropped than target blocks added).
    /// As soon as a pass fails that test the loop exits — this
    /// guarantees the helper terminates even when the
    /// density threshold would otherwise keep flagging the
    /// freshly-packed targets as sparse.
    ///
    /// `max_passes` of 0 returns 0 immediately. The loop bound
    /// caps worst-case work for pathological heaps.
    pub fn compact_old_gen_aggressive(
        &mut self,
        density_threshold: f64,
        max_passes: usize,
    ) -> usize {
        let mut total_moved = 0usize;
        for _ in 0..max_passes {
            let blocks_before = self.old_gen.block_count();
            let moved = self.compact_old_gen_physical(density_threshold);
            if moved == 0 {
                break;
            }
            total_moved = total_moved.saturating_add(moved);
            let blocks_after = self.old_gen.block_count();
            // Termination check: if compaction did not net-
            // shrink the block pool, no further progress is
            // possible -- continuing would just move the same
            // records between fresh targets indefinitely.
            if blocks_after >= blocks_before {
                break;
            }
        }
        total_moved
    }

    pub(crate) fn collection_exec_parts(
        &mut self,
    ) -> (
        &mut RootStack,
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
        &OldGenConfig,
        &NurseryConfig,
        &mut NurseryState,
    ) {
        let Self {
            config,
            stats,
            roots,
            objects,
            indexes,
            old_gen,
            nursery,
            ..
        } = self;
        (
            roots,
            objects,
            indexes,
            old_gen,
            stats,
            &config.old,
            &config.nursery,
            nursery,
        )
    }

    pub(crate) fn finished_reclaim_commit_parts(
        &mut self,
    ) -> (
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
    ) {
        let Self {
            objects,
            indexes,
            old_gen,
            stats,
            ..
        } = self;
        (objects, indexes, old_gen, stats)
    }

    pub(crate) fn allocation_commit_parts(
        &mut self,
    ) -> (
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
        &OldGenConfig,
    ) {
        let Self {
            config,
            objects,
            indexes,
            old_gen,
            stats,
            ..
        } = self;
        (objects, indexes, old_gen, stats, &config.old)
    }

    /// Return the heap configuration.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        let mut stats = self.storage_stats();
        self.runtime_state.apply_runtime_stats(&mut stats);
        stats
    }

    pub(crate) fn storage_stats(&self) -> HeapStats {
        let mut stats = self.stats;
        self.indexes.apply_storage_stats(&mut stats);
        // Phase 4: fold dirty card counts into the legacy
        // remembered_edges/remembered_owners counters so observers see
        // a unified view across the legacy Vec+HashSet path and the
        // per-block card-table fast path.
        self.indexes
            .apply_dirty_card_storage_stats(&mut stats, &self.old_gen);
        stats
    }

    pub(crate) fn runtime_finalizer_stats(&self) -> (u64, usize) {
        self.runtime_state.snapshot()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.runtime_state.runtime_work_status()
    }

    pub(crate) fn collector_shared_snapshot(&self) -> CollectorSharedSnapshot {
        self.collector.shared_snapshot()
    }

    /// Build a scheduler-visible collection plan from current heap state.
    pub fn plan_for(&self, kind: CollectionKind) -> CollectionPlan {
        crate::collector_policy::build_plan(
            kind,
            &self.objects,
            &self.stats,
            &self.config.nursery,
            &self.config.old,
            &self.old_gen,
        )
    }

    /// Recommend the next collection plan from current heap pressure.
    pub fn recommended_plan(&self) -> CollectionPlan {
        self.collector.recommended_plan()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.collector.recommended_background_plan()
    }

    pub(crate) fn refresh_recommended_plans(&self) {
        self.collector.refresh_cached_plans(
            &self.storage_stats(),
            &self.old_gen,
            &self.config.old,
            |kind| self.plan_for(kind),
        );
    }

    /// Return the phases traversed by the most recently executed collection.
    pub fn recent_phase_trace(&self) -> Vec<CollectionPhase> {
        self.collector.recent_phase_trace()
    }

    /// Return the most recently completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.collector.last_completed_plan()
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.collector.active_major_mark_plan()
    }

    /// Return current progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.collector.major_mark_progress()
    }

    /// Begin a persistent major-mark session for `plan`.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        CollectorRuntime::new(self).begin_major_mark(plan)
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        CollectorRuntime::new(self).advance_major_mark()
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).finish_major_collection()
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        CollectorRuntime::new(self).assist_major_mark(max_slices)
    }

    /// Advance one scheduler-style concurrent major-mark round using the plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        CollectorRuntime::new(self).poll_active_major_mark()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        CollectorRuntime::new(self).finish_active_major_collection_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        CollectorRuntime::new(self).commit_active_reclaim_if_ready()
    }

    /// Return logical old-generation region statistics.
    ///
    /// This is the legacy view: it reads from the regions vec
    /// that the major-cycle rebuild rewrites in place to
    /// renumber survivors into tightly-packed regions. The
    /// `hole_bytes` field shrinks after a major cycle as a
    /// side effect of the rebuild even though no bytes are
    /// physically moved. Prefer
    /// [`Heap::old_block_region_stats`] for the honest physical
    /// layout.
    pub fn old_region_stats(&self) -> Vec<OldRegionStats> {
        self.old_gen.region_stats()
    }

    /// Return the per-block old-generation statistics view.
    ///
    /// Each entry corresponds to one `OldBlock` in allocation
    /// order. Unlike [`Heap::old_region_stats`], this view is
    /// computed directly from the per-block live/used counters
    /// the sweep rebuild maintains, so the reported
    /// `hole_bytes` reflect the *physical* layout of the heap:
    /// they only shrink when bytes are actually moved (via
    /// physical compaction), not as a side effect of logical
    /// renumbering.
    ///
    /// This is the long-term replacement for `old_region_stats`
    /// once the remaining `lib_test.rs` assertions that depend
    /// on the logical-compaction shrink contract are migrated.
    /// New observers should use this method.
    pub fn old_block_region_stats(&self) -> Vec<OldRegionStats> {
        self.old_gen.block_region_stats()
    }

    /// Return the currently selected old-region compaction candidates.
    pub fn major_region_candidates(&self) -> Vec<OldRegionStats> {
        let OldGenPlanSelection { candidates, .. } =
            self.old_gen.major_plan_selection(&self.config.old);
        candidates
    }

    /// Return the same compaction-candidate ranking as
    /// [`Heap::major_region_candidates`], but computed against the
    /// per-block view rather than the legacy regions vec.
    ///
    /// The returned `region_index` fields refer to block indices,
    /// not logical region indices, and the underlying selection
    /// runs the identical heuristic
    /// (`hole_bytes >= selective_reclaim_threshold_bytes`, ranked
    /// by compaction efficiency, capped at
    /// `compaction_candidate_limit` and
    /// `max_compaction_bytes_per_cycle`).
    ///
    /// This is the long-term replacement for
    /// `major_region_candidates`. Today its output is observational
    /// only — the rebuild path still consumes the legacy
    /// region-indexed selection. Tests that just want to verify
    /// candidate ranking can already use this method; tests that
    /// feed indices back into a manual `CollectionPlan` still need
    /// the legacy entry point.
    pub fn major_block_candidates(&self) -> Vec<OldRegionStats> {
        let OldGenPlanSelection { candidates, .. } =
            self.old_gen.block_plan_selection(&self.config.old);
        candidates
    }

    /// Number of live objects currently tracked by the heap.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.runtime_state.pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> u64 {
        self.runtime_state.drain_pending_finalizers()
    }

    /// Number of remembered old-to-young edges currently tracked.
    pub fn remembered_edge_count(&self) -> usize {
        self.indexes.remembered.edges.len()
    }

    #[cfg(test)]
    pub(crate) fn remembered_owner_count(&self) -> usize {
        self.indexes.remembered.owners.len()
    }

    /// Sum live_bytes and object_count across every old-gen block
    /// in the pool. Used by the OldRegion unification tests to
    /// assert that the block-side accounting (step 2) mirrors the
    /// region-side accounting for the same allocation sequence.
    #[cfg(test)]
    pub(crate) fn inspect_old_gen_block_accounting_for_test(&self) -> (usize, usize) {
        let live: usize = self
            .old_gen
            .blocks()
            .iter()
            .map(|block| block.live_bytes())
            .sum();
        let count: usize = self
            .old_gen
            .blocks()
            .iter()
            .map(|block| block.object_count())
            .sum();
        (live, count)
    }

    /// Total dirty cards across every old-gen block. Combined with
    /// `remembered_edge_count()` this gives the full picture of pending
    /// old-to-young roots between collections.
    pub fn dirty_card_count(&self) -> usize {
        self.old_gen.dirty_card_count()
    }

    /// Total number of pending old-to-young roots, summed across both
    /// the legacy `RememberedSetState` (used for non-block-backed
    /// owners) and the per-block dirty-card tables (Phase 4 fast path).
    pub fn total_remembered_count(&self) -> usize {
        self.remembered_edge_count().saturating_add(self.dirty_card_count())
    }

    /// Number of recent barrier events retained for diagnostics.
    pub fn barrier_event_count(&self) -> usize {
        self.recent_barrier_events.len()
    }

    /// Recorded recent barrier events retained for diagnostics.
    pub fn recent_barrier_events(&self) -> &[BarrierEvent] {
        &self.recent_barrier_events
    }

    /// Cumulative write-barrier traffic counters.
    ///
    /// The returned [`crate::stats::BarrierStats`] reports the
    /// number of post-write and SATB pre-write barriers the
    /// heap has recorded over its entire lifetime. These
    /// counters are monotonic, so callers can subtract two
    /// snapshots to attribute barrier traffic to one interval.
    /// The recent-events ring buffer is bounded for diagnostic
    /// inspection; these counters are unbounded for telemetry.
    ///
    /// # Example
    ///
    /// ```
    /// use neovm_gc::{Heap, HeapConfig};
    ///
    /// let heap = Heap::new(HeapConfig::default());
    /// let stats = heap.barrier_stats();
    /// assert_eq!(stats.post_write, 0);
    /// assert_eq!(stats.satb_pre_write, 0);
    /// ```
    pub fn barrier_stats(&self) -> crate::stats::BarrierStats {
        self.barrier_stats
    }

    /// Reset cumulative barrier traffic counters back to zero.
    /// Recent diagnostic events retained in the bounded ring
    /// buffer are left untouched.
    pub fn clear_barrier_stats(&mut self) {
        self.barrier_stats = crate::stats::BarrierStats::default();
    }

    #[cfg(test)]
    pub(crate) fn root_slot_count(&self) -> usize {
        self.roots.len()
    }

    pub(crate) fn root_stack_ptr(&mut self) -> NonNull<RootStack> {
        NonNull::from(&mut self.roots)
    }

    /// Create a mutator bound to this heap.
    pub fn mutator(&mut self) -> Mutator<'_> {
        Mutator::new(self)
    }

    /// Create a collector-side runtime bound to this heap.
    pub fn collector_runtime(&mut self) -> CollectorRuntime<'_> {
        CollectorRuntime::new(self)
    }

    /// Create a background collection service loop bound to this heap.
    pub fn background_service(
        &mut self,
        config: BackgroundCollectorConfig,
    ) -> BackgroundService<'_> {
        self.collector_runtime().background_service(config)
    }

    /// Convert this heap into a shared synchronized heap wrapper.
    pub fn into_shared(self) -> SharedHeap {
        SharedHeap::from_heap(self)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).collect(kind)
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).execute_plan(plan)
    }

    pub(crate) fn push_barrier_event(
        &mut self,
        kind: BarrierKind,
        owner: GcErased,
        slot: Option<usize>,
        old_value: Option<GcErased>,
        new_value: Option<GcErased>,
    ) {
        const MAX_BARRIER_EVENTS: usize = 1024;

        match kind {
            BarrierKind::PostWrite => {
                self.barrier_stats.post_write =
                    self.barrier_stats.post_write.saturating_add(1);
            }
            BarrierKind::SatbPreWrite => {
                self.barrier_stats.satb_pre_write =
                    self.barrier_stats.satb_pre_write.saturating_add(1);
            }
        }

        self.recent_barrier_events.push(BarrierEvent {
            kind,
            owner: unsafe { crate::root::Gc::from_erased(owner) },
            slot,
            old_value: old_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
            new_value: new_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
        });
        if self.recent_barrier_events.len() > MAX_BARRIER_EVENTS {
            let overflow = self.recent_barrier_events.len() - MAX_BARRIER_EVENTS;
            self.recent_barrier_events.drain(..overflow);
        }
    }

    pub(crate) fn record_remembered_edge_if_needed(
        &mut self,
        owner: GcErased,
        new_value: Option<GcErased>,
    ) {
        self.indexes.record_remembered_edge_if_needed(
            &self.objects,
            &self.old_gen,
            owner,
            new_value,
        );
    }

    pub(crate) fn prepared_full_reclaim_active(&self) -> bool {
        self.collector.has_prepared_full_reclaim()
    }

    pub(crate) fn descriptor_for<T: Trace + 'static>(&mut self) -> &'static TypeDesc {
        let type_id = TypeId::of::<T>();
        self.descriptors
            .entry(type_id)
            .or_insert_with(|| Box::leak(Box::new(fixed_type_desc::<T>())))
    }

    pub(crate) fn allocation_pressure_plan(
        &self,
        space: SpaceKind,
        bytes: usize,
    ) -> Option<CollectionPlan> {
        crate::collector_policy::allocation_pressure_plan(
            &self.stats,
            &self.config.nursery,
            &self.config.pinned,
            &self.config.large,
            space,
            bytes,
            |kind| self.plan_for(kind),
        )
    }

    pub(crate) fn record_collection_stats(&mut self, cycle: CollectionStats) {
        self.stats.collections.saturating_add_assign(cycle);
        if cycle.pause_nanos > 0 {
            self.pause_stats.record(cycle.pause_nanos);
        }
        if cycle.major_collections > 0 {
            let live_after = self.storage_stats().total_live_bytes();
            self.pacer.record_completed_cycle(&cycle, live_after);
        }
        if cycle.minor_collections > 0 {
            // Reset the pacer's nursery soft-trigger counter so the
            // next early-minor heuristic measures freshly allocated
            // bytes only.
            self.pacer.record_completed_minor_cycle();
        }
    }

    /// Return a snapshot of recent stop-the-world pause statistics (P50/P95/P99
    /// of pause nanoseconds over a bounded rolling window).
    pub fn pause_histogram(&self) -> PauseHistogram {
        self.pause_stats.snapshot()
    }

    #[allow(dead_code)]
    pub(crate) fn pause_stats_handle(&self) -> PauseStatsHandle {
        self.pause_stats.clone()
    }

    /// Snapshot the adaptive pacer's current model.
    pub fn pacer_stats(&self) -> PacerStats {
        self.pacer.stats()
    }

    /// Return a clone of the pacer handle. Cheap; the inner state is
    /// shared via `Arc<Mutex<...>>`.
    pub fn pacer(&self) -> Pacer {
        self.pacer.clone()
    }

    /// Override the pacer's configuration in place. Preserves the
    /// pacer's accumulated runtime state (EWMA estimates, observed
    /// cycles, next-trigger threshold) so production callers can
    /// retune the pacer without losing its history.
    ///
    /// All cloned [`Pacer`] handles see the new config because they
    /// share the same `Arc<Mutex<PacerState>>`.
    pub fn set_pacer_config(&mut self, config: PacerConfig) {
        self.pacer.update_config(config);
    }

    #[cfg(test)]
    pub(crate) fn contains<T>(&self, gc: crate::root::Gc<T>) -> bool {
        self.indexes
            .object_index
            .contains_key(&gc.erase().object_key())
    }

    #[cfg(test)]
    pub(crate) fn finalizable_candidate_count(&self) -> usize {
        self.indexes.finalizable_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn weak_candidate_count(&self) -> usize {
        self.indexes.weak_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn ephemeron_candidate_count(&self) -> usize {
        self.indexes.ephemeron_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn space_of<T>(&self, gc: crate::root::Gc<T>) -> Option<SpaceKind> {
        self.indexes
            .object_index
            .get(&gc.erase().object_key())
            .map(|&index| self.objects[index].space())
    }
}

/// Drain pending finalizers at the controlled boundary of `Heap` drop so
/// that any arena- or old-block-backed `ObjectRecord`s sitting in
/// `RuntimeState::pending_finalizers` run their payload `drop_in_place`
/// while the backing buffers in `NurseryState` / `OldGenState` are still
/// alive.
///
/// Without this, a `SharedHeap` clone of `RuntimeStateHandle` can keep
/// the `RuntimeState` alive past `Heap`'s drop. When that Arc finally
/// hits zero, the pending `ObjectRecord`s try to deref headers in
/// arena or old-block buffers that have already been freed as part
/// of `Heap`'s field-order drop sequence.
impl Drop for Heap {
    fn drop(&mut self) {
        let _ = self.runtime_state.drain_pending_finalizers();
    }
}
