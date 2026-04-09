use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::BarrierKind;
use crate::collector_exec::collect_global_sources;
use crate::collector_state::{CollectorSharedSnapshot, CollectorStateHandle};
use crate::descriptor::{GcErased, Trace, TypeDesc, fixed_type_desc};
use crate::index_state::HeapIndexState;
use crate::mutator::Mutator;
use crate::object::{ObjectRecord, SpaceKind};
use crate::pacer::{Pacer, PacerConfig, PacerStats};
use crate::pause_stats::{PauseHistogram, PauseStatsHandle};
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan,
    MajorMarkProgress, RuntimeWorkStatus,
};
use crate::runtime::CollectorRuntime;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, NurseryState, OldGenConfig, OldGenPlanSelection,
    OldGenState, PinnedSpaceConfig,
};
use crate::stats::{CollectionStats, HeapStats, OldRegionStats};
use core::any::TypeId;
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

/// Crate-internal heap owner.
///
/// `HeapCore` owns every field the heap manages: configuration,
/// statistics, descriptors, the object log, runtime state,
/// indexes, old-generation pool, collector handle, pacer,
/// barrier stats, and the nursery. It is the sole owner of
/// the heap's storage; the public [`Heap`] type is a
/// `#[repr(transparent)]` newtype around a `HeapCore`.
///
/// Field order matters: `ObjectRecord` storage (both the live
/// `objects` vec and the `runtime_state` pending-finalizer
/// queue) must drop BEFORE the `NurseryState` that backs
/// their arena allocations, otherwise arena-owned records
/// would run their payload `Drop` implementations against
/// already-freed buffers.
///
/// The multi-mutator refactor (DESIGN.md Appendix A) will
/// eventually wrap `HeapCore` in `Arc<RwLock<HeapCore>>`
/// inside [`Heap`] so multiple `Mutator` instances can
/// coexist against the same heap. This commit introduces the
/// split as a concrete type so later commits can slot the
/// lock in without churning the interior field layout.
#[derive(Debug)]
pub(crate) struct HeapCore {
    config: HeapConfig,
    stats: HeapStats,
    descriptors: HashMap<TypeId, &'static TypeDesc>,
    // --- record storage (drops first, before nursery) ---
    objects: Vec<ObjectRecord>,
    runtime_state: RuntimeStateHandle,
    // --- index and collector bookkeeping ---
    indexes: HeapIndexState,
    old_gen: OldGenState,
    collector: CollectorStateHandle,
    pause_stats: PauseStatsHandle,
    pacer: Pacer,
    /// Cumulative physical old-gen compaction counters. Updated
    /// by `compact_old_gen_physical` after every call that
    /// actually moves at least one record.
    compaction_stats: crate::stats::CompactionStats,
    /// Cumulative write-barrier traffic counters. The
    /// backing store is atomic so the barrier hot path can
    /// bump the counters with a relaxed fetch-add, avoiding
    /// the heap write lock entirely. Observers read a
    /// [`crate::stats::BarrierStats`] snapshot via
    /// [`crate::stats::AtomicBarrierStats::snapshot`].
    barrier_stats: crate::stats::AtomicBarrierStats,
    // --- arena buffers (drops last, after all records) ---
    /// Bump-pointer semispace nursery arenas.
    nursery: NurseryState,
}

// SAFETY: `HeapCore` owns all heap allocations and its raw pointers are internal references into
// that owned storage or static descriptors. Sending a `HeapCore` to another thread does not
// invalidate those pointers. Concurrent access is still not allowed without external
// synchronization, so `HeapCore` is `Send` but intentionally not `Sync`.
unsafe impl Send for HeapCore {}

/// Global heap object.
///
/// `Heap` owns an `Arc<RwLock<HeapCore>>` so multiple
/// `Mutator` instances can coexist against the same heap.
/// Each mutator briefly acquires the write lock to perform
/// allocation, barrier, or collection work, and drops it at
/// the end of the operation. The hot-path TLAB bump still
/// lives on the per-mutator nursery slab inside the
/// mutator's local state and does not need to acquire the
/// lock on hit.
///
/// `Heap` is `Clone` via `Arc::clone` — passing the heap to
/// another thread or storing additional handles is cheap.
/// The cloned handles all reference the same underlying
/// `HeapCore`.
#[derive(Clone, Debug)]
pub struct Heap {
    core: std::sync::Arc<std::sync::RwLock<HeapCore>>,
}

impl Heap {
    /// Create a new heap with `config`.
    pub fn new(config: HeapConfig) -> Self {
        Self {
            core: std::sync::Arc::new(std::sync::RwLock::new(HeapCore::new(config))),
        }
    }

    /// Convert this heap into a shared synchronized heap wrapper.
    pub fn into_shared(self) -> SharedHeap {
        SharedHeap::from_heap(self)
    }

    /// Acquire a write guard on the underlying `HeapCore`.
    /// Used by every mutating heap operation. The guard is
    /// dropped when the returned value goes out of scope.
    #[inline]
    pub(crate) fn write_core(&self) -> std::sync::RwLockWriteGuard<'_, HeapCore> {
        self.core.write().expect("heap core lock poisoned")
    }

    /// Acquire a read guard on the underlying `HeapCore`.
    /// Used by read-only heap accessors and by tests that
    /// need to traverse heap-owned data structures across
    /// multiple statements.
    #[inline]
    pub(crate) fn read_core(&self) -> std::sync::RwLockReadGuard<'_, HeapCore> {
        self.core.read().expect("heap core lock poisoned")
    }

    /// Crate-internal `Arc` clone helper.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn clone_arc(&self) -> std::sync::Arc<std::sync::RwLock<HeapCore>> {
        std::sync::Arc::clone(&self.core)
    }

    // -- Public forwarders --------------------------------------------------
    //
    // Every method below is a thin wrapper around the matching
    // method on `HeapCore`. We cannot use `Deref`/`DerefMut` to
    // auto-forward because `HeapCore` is `pub(crate)` and cannot
    // appear in a public trait impl's associated type. The
    // forwarders preserve the exact public signature the heap
    // exposed before the split so external callers see no
    // behavioral change.

    /// Heap configuration. Returned by value because the
    /// underlying field lives behind the heap lock.
    pub fn config(&self) -> HeapConfig {
        *self.read_core().config()
    }

    /// Snapshot the current heap statistics.
    pub fn stats(&self) -> HeapStats {
        self.read_core().stats()
    }

    /// Runtime-side follow-up work.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.read_core().runtime_work_status()
    }

    /// Run physical old-gen compaction.
    pub fn compact_old_gen_physical(&self, density_threshold: f64) -> usize {
        let mut roots = crate::root::RootStack::default();
        self.write_core()
            .compact_old_gen_physical(&mut roots, density_threshold)
    }

    /// Run targeted block compaction.
    pub fn compact_old_gen_blocks(&self, block_indices: &[usize]) -> usize {
        let mut roots = crate::root::RootStack::default();
        self.write_core()
            .compact_old_gen_blocks(&mut roots, block_indices)
    }

    /// Cumulative compaction stats.
    pub fn compaction_stats(&self) -> crate::stats::CompactionStats {
        self.read_core().compaction_stats()
    }

    /// Reset compaction stats.
    pub fn clear_compaction_stats(&self) {
        self.write_core().clear_compaction_stats();
    }

    /// Nursery fill ratio.
    pub fn nursery_fill_ratio(&self) -> f64 {
        self.read_core().nursery_fill_ratio()
    }

    /// Old-generation fragmentation ratio.
    pub fn old_gen_fragmentation_ratio(&self) -> f64 {
        self.read_core().old_gen_fragmentation_ratio()
    }

    /// Opportunistic compaction trigger.
    pub fn compact_old_gen_if_fragmented(
        &self,
        fragmentation_threshold: f64,
    ) -> (f64, usize) {
        let mut roots = crate::root::RootStack::default();
        self.write_core()
            .compact_old_gen_if_fragmented(&mut roots, fragmentation_threshold)
    }

    /// Predicate-only fragmentation check.
    pub fn should_compact_old_gen(&self, fragmentation_threshold: f64) -> bool {
        self.read_core().should_compact_old_gen(fragmentation_threshold)
    }

    /// Aggressive compaction wrapper.
    pub fn compact_old_gen_aggressive(
        &self,
        density_threshold: f64,
        max_passes: usize,
    ) -> usize {
        let mut roots = crate::root::RootStack::default();
        self.write_core()
            .compact_old_gen_aggressive(&mut roots, density_threshold, max_passes)
    }

    /// Build a scheduler-visible collection plan.
    pub fn plan_for(&self, kind: CollectionKind) -> CollectionPlan {
        self.read_core().plan_for(kind)
    }

    /// Recommend the next collection plan.
    pub fn recommended_plan(&self) -> CollectionPlan {
        self.read_core().recommended_plan()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.read_core().recommended_background_plan()
    }

    /// Phases traversed by the most recently executed collection.
    pub fn recent_phase_trace(&self) -> Vec<CollectionPhase> {
        self.read_core().recent_phase_trace()
    }

    /// Most recently completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.read_core().last_completed_plan()
    }

    /// Active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.read_core().active_major_mark_plan()
    }

    /// Active major-mark progress, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.read_core().major_mark_progress()
    }

    /// Begin a persistent major-mark session.
    pub fn begin_major_mark(&self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.collector_runtime().begin_major_mark(plan)
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&self) -> Result<MajorMarkProgress, AllocError> {
        self.collector_runtime().advance_major_mark()
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&self) -> Result<CollectionStats, AllocError> {
        self.collector_runtime().finish_major_collection()
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.collector_runtime().assist_major_mark(max_slices)
    }

    /// Advance one scheduler-style concurrent major-mark round.
    pub fn poll_active_major_mark(&self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.collector_runtime().poll_active_major_mark()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.collector_runtime().finish_active_major_collection_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.collector_runtime().commit_active_reclaim_if_ready()
    }

    /// Per-block old-generation statistics.
    pub fn old_region_stats(&self) -> Vec<OldRegionStats> {
        self.read_core().old_region_stats()
    }

    /// Per-block old-generation statistics view.
    pub fn old_block_region_stats(&self) -> Vec<OldRegionStats> {
        self.read_core().old_block_region_stats()
    }

    /// Currently selected old-block compaction candidates.
    pub fn major_block_candidates(&self) -> Vec<OldRegionStats> {
        self.read_core().major_block_candidates()
    }

    /// Number of live objects currently tracked by the heap.
    pub fn object_count(&self) -> usize {
        self.read_core().object_count()
    }

    /// Number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.read_core().pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> u64 {
        self.read_core().drain_pending_finalizers()
    }

    /// Run at most `max` queued finalizers.
    pub fn drain_pending_finalizers_bounded(&self, max: usize) -> u64 {
        self.read_core().drain_pending_finalizers_bounded(max)
    }

    /// Number of remembered edges tracked by the explicit fallback path.
    pub fn remembered_edge_count(&self) -> usize {
        self.read_core().remembered_edge_count()
    }

    /// Total dirty cards across every old-gen block.
    pub fn dirty_card_count(&self) -> usize {
        self.read_core().dirty_card_count()
    }

    /// Unified count of pending old-to-young roots.
    pub fn total_remembered_count(&self) -> usize {
        self.read_core().total_remembered_count()
    }

    /// Cumulative write-barrier traffic counters.
    pub fn barrier_stats(&self) -> crate::stats::BarrierStats {
        self.read_core().barrier_stats()
    }

    /// Reset cumulative barrier traffic counters.
    pub fn clear_barrier_stats(&self) {
        self.read_core().clear_barrier_stats();
    }

    /// Create a mutator bound to this heap.
    ///
    /// Takes `&self` so multiple mutators can coexist
    /// against the same heap at the type level. Each mutator
    /// owns its own [`crate::mutator::MutatorLocal`] and
    /// briefly acquires the heap core write lock per
    /// operation.
    pub fn mutator(&self) -> Mutator<'_> {
        Mutator::new(self)
    }

    /// Create a collector-side runtime guard bound to this
    /// heap. The returned guard holds an exclusive write
    /// lock on the heap core for its entire lifetime, so
    /// only one outstanding `HeapCollectorRuntime` can exist
    /// at a time.
    pub fn collector_runtime(&self) -> HeapCollectorRuntime<'_> {
        HeapCollectorRuntime::new(self.write_core())
    }

    /// Create a background collection service loop bound to this heap.
    pub fn background_service(
        &self,
        config: BackgroundCollectorConfig,
    ) -> BackgroundService<'_> {
        BackgroundService::from_runtime_guard(self.collector_runtime(), config)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        self.collector_runtime().collect(kind)
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        self.collector_runtime().execute_plan(plan)
    }

    /// Snapshot of recent stop-the-world pause statistics.
    pub fn pause_histogram(&self) -> PauseHistogram {
        self.read_core().pause_histogram()
    }

    /// Snapshot of the adaptive pacer's current model.
    pub fn pacer_stats(&self) -> PacerStats {
        self.read_core().pacer_stats()
    }

    /// Clone of the pacer handle.
    pub fn pacer(&self) -> Pacer {
        self.read_core().pacer()
    }

    /// Override the pacer's configuration in place.
    pub fn set_pacer_config(&self, config: PacerConfig) {
        self.write_core().set_pacer_config(config);
    }

    // -- Crate-internal forwarders ------------------------------------------
    //
    // Internal helpers reachable from collector code that only
    // has a `&Heap` (typically from a shared heap lock guard).
    // These mirror the `pub(crate) fn` surface on `HeapCore` so
    // the collector can keep calling `heap.xyz()` without first
    // unwrapping to `heap.core().xyz()`.

    pub(crate) fn storage_stats(&self) -> HeapStats {
        self.read_core().storage_stats()
    }

    pub(crate) fn runtime_finalizer_stats(&self) -> (u64, usize) {
        self.read_core().runtime_finalizer_stats()
    }

    pub(crate) fn runtime_state_handle(&self) -> RuntimeStateHandle {
        self.read_core().runtime_state_handle()
    }

    pub(crate) fn collector_handle(&self) -> CollectorStateHandle {
        self.read_core().collector_handle()
    }

    pub(crate) fn collector_shared_snapshot(&self) -> CollectorSharedSnapshot {
        self.read_core().collector_shared_snapshot()
    }

    // -- Test-only forwarders ----------------------------------------------

    #[cfg(test)]
    pub(crate) fn contains<T>(&self, gc: crate::root::Gc<T>) -> bool {
        self.read_core().contains(gc)
    }

    #[cfg(test)]
    pub(crate) fn finalizable_candidate_count(&self) -> usize {
        self.read_core().finalizable_candidate_count()
    }

    #[cfg(test)]
    pub(crate) fn weak_candidate_count(&self) -> usize {
        self.read_core().weak_candidate_count()
    }

    #[cfg(test)]
    pub(crate) fn ephemeron_candidate_count(&self) -> usize {
        self.read_core().ephemeron_candidate_count()
    }

    #[cfg(test)]
    pub(crate) fn space_of<T>(&self, gc: crate::root::Gc<T>) -> Option<SpaceKind> {
        self.read_core().space_of(gc)
    }

    /// Test-only build a write-locked collector runtime
    /// that borrows the supplied external `MutatorLocal`.
    #[cfg(test)]
    pub(crate) fn collector_runtime_with_local<'a>(
        &'a self,
        local: &'a mut crate::mutator::MutatorLocal,
    ) -> HeapCollectorRuntimeWithLocal<'a> {
        HeapCollectorRuntimeWithLocal {
            guard: self.write_core(),
            local,
        }
    }

    #[cfg(test)]
    pub(crate) fn remembered_owner_count(&self) -> usize {
        self.read_core().remembered_owner_count()
    }

    #[cfg(test)]
    pub(crate) fn inspect_old_gen_block_accounting_for_test(&self) -> (usize, usize) {
        self.read_core().inspect_old_gen_block_accounting_for_test()
    }
}

/// Guard type returned by [`Heap::collector_runtime`] that
/// holds an exclusive write guard on the heap core and a
/// scratch `MutatorLocal` for the duration of collector
/// operations. Each method builds a fresh
/// [`CollectorRuntime`] against the held borrows and runs
/// the operation through it.
#[derive(Debug)]
pub struct HeapCollectorRuntime<'a> {
    guard: std::sync::RwLockWriteGuard<'a, HeapCore>,
    local: crate::mutator::MutatorLocal,
}

impl<'a> HeapCollectorRuntime<'a> {
    fn new(guard: std::sync::RwLockWriteGuard<'a, HeapCore>) -> Self {
        Self {
            guard,
            local: crate::mutator::MutatorLocal::default(),
        }
    }

    /// Build the inner `CollectorRuntime` from the held heap
    /// core borrow plus this guard's scratch local.
    pub(crate) fn runtime(&mut self) -> CollectorRuntime<'_> {
        CollectorRuntime::with_local(&mut self.guard, &mut self.local)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        self.runtime().collect(kind)
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        self.runtime().execute_plan(plan)
    }

    /// Begin a persistent major-mark session.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.runtime().begin_major_mark(plan)
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        self.runtime().advance_major_mark()
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.runtime().assist_major_mark(max_slices)
    }

    /// Advance one scheduler-style concurrent major-mark round.
    pub fn poll_active_major_mark(
        &mut self,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.runtime().poll_active_major_mark()
    }

    /// Finish the current persistent major-mark session.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        self.runtime().finish_major_collection()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.runtime().finish_active_major_collection_if_ready()
    }

    /// Prepare reclaim for the active major collection.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.runtime().prepare_active_reclaim_if_needed()
    }

    /// Commit the active major collection.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.runtime().commit_active_reclaim_if_ready()
    }

    /// Service one background collection round.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.runtime().service_background_collection_round()
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        self.guard.stats()
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.guard.pending_finalizer_count()
    }

    /// Drain queued finalizers.
    pub fn drain_pending_finalizers(&mut self) -> u64 {
        self.guard.drain_pending_finalizers()
    }

    /// Drain up to `max` queued finalizers.
    pub fn drain_pending_finalizers_bounded(&mut self, max: usize) -> u64 {
        self.guard.drain_pending_finalizers_bounded(max)
    }

    /// Active major-mark plan, if any.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.guard.active_major_mark_plan()
    }

    /// Active major-mark progress, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.guard.major_mark_progress()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.guard.recommended_background_plan()
    }

    /// Last completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.guard.last_completed_plan()
    }

    /// Runtime-side follow-up work.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.guard.runtime_work_status()
    }

    /// Heap configuration.
    pub fn config(&self) -> HeapConfig {
        *self.guard.config()
    }

    /// Prepare a typed allocation pressure check.
    pub fn prepare_typed_allocation<T: crate::descriptor::Trace + 'static>(
        &mut self,
    ) -> Result<(), AllocError> {
        self.runtime().prepare_typed_allocation::<T>()
    }

    /// Test-only: service allocation pressure for the given space and bytes.
    #[cfg(test)]
    pub(crate) fn service_allocation_pressure(
        &mut self,
        space: crate::object::SpaceKind,
        bytes: usize,
    ) -> Result<(), AllocError> {
        self.runtime().service_allocation_pressure(space, bytes)
    }

    /// Convert this collector runtime guard into a background
    /// service loop.
    pub fn background_service(
        self,
        config: BackgroundCollectorConfig,
    ) -> BackgroundService<'a> {
        BackgroundService::from_runtime_guard(self, config)
    }

    /// Crate-internal access to the underlying `HeapCore`
    /// for callers that need to inspect heap-owned state
    /// while the guard is held.
    #[allow(dead_code)]
    pub(crate) fn heap_core(&self) -> &HeapCore {
        &self.guard
    }
}

impl crate::background::BackgroundCollectionRuntime for HeapCollectorRuntime<'_> {
    fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        HeapCollectorRuntime::active_major_mark_plan(self)
    }

    fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        HeapCollectorRuntime::recommended_background_plan(self)
    }

    fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        HeapCollectorRuntime::begin_major_mark(self, plan)
    }

    fn poll_background_mark_round(
        &mut self,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        HeapCollectorRuntime::poll_active_major_mark(self)
    }

    fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        HeapCollectorRuntime::prepare_active_reclaim_if_needed(self)
    }

    fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        HeapCollectorRuntime::finish_active_major_collection_if_ready(self)
    }

    fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        HeapCollectorRuntime::commit_active_reclaim_if_ready(self)
    }
}

/// Test-only guard returned by
/// [`Heap::collector_runtime_with_local`] that borrows an
/// external `MutatorLocal` so the caller can hand out a
/// root stack pointer before constructing the runtime.
#[cfg(test)]
#[derive(Debug)]
pub struct HeapCollectorRuntimeWithLocal<'a> {
    guard: std::sync::RwLockWriteGuard<'a, HeapCore>,
    local: &'a mut crate::mutator::MutatorLocal,
}

#[cfg(test)]
impl<'a> HeapCollectorRuntimeWithLocal<'a> {
    pub(crate) fn alloc_typed_scoped<'scope, 'handle_heap, T: crate::descriptor::Trace + 'static>(
        &mut self,
        scope: &mut crate::root::HandleScope<'scope, 'handle_heap>,
        value: T,
    ) -> Result<crate::root::Root<'scope, T>, AllocError> {
        CollectorRuntime::with_local(&mut self.guard, &mut *self.local)
            .alloc_typed_scoped(scope, value)
    }

    pub(crate) fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        CollectorRuntime::with_local(&mut self.guard, &mut *self.local).begin_major_mark(plan)
    }

    pub(crate) fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::with_local(&mut self.guard, &mut *self.local)
            .finish_major_collection()
    }
}

impl HeapCore {
    /// Create a new heap core with `config`.
    pub(crate) fn new(config: HeapConfig) -> Self {
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
                remembered_explicit_edges: 0,
                remembered_dirty_cards: 0,
                remembered_explicit_owners: 0,
                remembered_dirty_card_owners: 0,
                old_gen_used_bytes: 0,
                finalizable_candidates: 0,
                weak_candidates: 0,
                ephemeron_candidates: 0,
                finalizers_run: 0,
                pending_finalizers: 0,
            },
            config,
            descriptors: HashMap::default(),
            objects: Vec::new(),
            indexes: HeapIndexState::default(),
            old_gen: OldGenState::default(),
            runtime_state: RuntimeStateHandle::default(),
            collector: CollectorStateHandle::default(),
            pause_stats: PauseStatsHandle::new(),
            pacer,
            compaction_stats: crate::stats::CompactionStats::default(),
            barrier_stats: crate::stats::AtomicBarrierStats::new(),
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

    /// Build the list of global trace sources the collector
    /// walks alongside mutator roots. Mutator roots now live
    /// on per-mutator `MutatorLocal` instances; this helper
    /// takes one root stack and pairs it with the heap's
    /// permanent sources (immortal objects, etc.).
    pub(crate) fn global_sources_with_roots(
        &self,
        roots: &crate::root::RootStack,
    ) -> Vec<GcErased> {
        collect_global_sources(roots, &self.objects)
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
    pub(crate) fn compact_old_gen_physical(
        &mut self,
        roots: &mut crate::root::RootStack,
        density_threshold: f64,
    ) -> usize {
        let runtime_state = self.runtime_state.clone();
        let block_count_before = self.old_gen.block_count();
        let Self {
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

    /// Physical old-gen compaction targeting an explicit set of
    /// block indices.
    ///
    /// Unlike [`Heap::compact_old_gen_physical`], which scans for
    /// sparse blocks via the density threshold, this method
    /// compacts exactly the blocks named in `block_indices` —
    /// every surviving record in those blocks is evacuated into
    /// freshly-created target blocks, inbound references are
    /// rewritten via the forwarding map, and the now-empty source
    /// blocks are reclaimed by the post-compact rebuild. Block
    /// indices not currently present in the pool are silently
    /// skipped.
    ///
    /// This is the manual-plan compaction surface: callers that
    /// know exactly which blocks they want compacted (typically
    /// from a previous `Heap::major_block_candidates` /
    /// `CollectionPlan::selected_old_blocks` snapshot) can pass
    /// the indices in directly. Returns the number of records
    /// physically evacuated.
    pub(crate) fn compact_old_gen_blocks(
        &mut self,
        roots: &mut crate::root::RootStack,
        block_indices: &[usize],
    ) -> usize {
        if block_indices.is_empty() {
            return 0;
        }
        let runtime_state = self.runtime_state.clone();
        let block_count_before = self.old_gen.block_count();
        let candidate_set: std::collections::HashSet<usize> =
            block_indices.iter().copied().collect();
        let Self {
            objects,
            indexes,
            old_gen,
            config,
            ..
        } = self;
        let old_config = &config.old;
        let forwarding = crate::reclaim::compact_specific_old_blocks(
            objects,
            old_gen,
            old_config,
            &candidate_set,
        );
        let moved = forwarding.len();
        if moved == 0 {
            return 0;
        }
        crate::spaces::nursery::relocate_roots_and_edges(roots, objects, indexes, &forwarding);
        let block_count_after_evacuation = old_gen.block_count();
        crate::reclaim::rebuild_line_marks_and_reclaim_empty_old_blocks(
            objects,
            old_gen,
            &runtime_state,
        );
        let block_count_after_rebuild = old_gen.block_count();
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
    pub(crate) fn compact_old_gen_if_fragmented(
        &mut self,
        roots: &mut crate::root::RootStack,
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
        let moved = self.compact_old_gen_physical(roots, density);
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
    pub(crate) fn compact_old_gen_aggressive(
        &mut self,
        roots: &mut crate::root::RootStack,
        density_threshold: f64,
        max_passes: usize,
    ) -> usize {
        let mut total_moved = 0usize;
        for _ in 0..max_passes {
            let blocks_before = self.old_gen.block_count();
            let moved = self.compact_old_gen_physical(roots, density_threshold);
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
            objects,
            indexes,
            old_gen,
            nursery,
            ..
        } = self;
        (
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
        // Fold dirty card counts into the unified
        // remembered_edges / remembered_owners counters so
        // observers see one combined view across the explicit
        // Vec+HashSet fallback path and the per-block
        // card-table fast path. The split counters
        // (remembered_explicit_*, remembered_dirty_card_*)
        // remain available for callers that want to attribute
        // pressure to one specific path.
        self.indexes
            .apply_dirty_card_storage_stats(&mut stats, &self.old_gen);
        // Cache the old-gen block bump cursor sum into the shared
        // stats surface so `SharedHeap::old_gen_fragmentation_ratio`
        // can read it from the cached snapshot without taking the
        // heap lock. The ratio is reconstructed as
        // `(old_gen_used_bytes - old.live_bytes) / old_gen_used_bytes`.
        stats.old_gen_used_bytes = self.old_gen.total_used_bytes();
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

    /// Return per-block old-generation statistics. Each entry
    /// corresponds to one `OldBlock` in allocation order; the
    /// `region_index` field carries the block index.
    ///
    /// `old_region_stats` and [`Heap::old_block_region_stats`]
    /// are aliases for the same per-block view — the legacy
    /// region-side reader has been retired. New observers
    /// should use either name; both are stable.
    pub fn old_region_stats(&self) -> Vec<OldRegionStats> {
        self.old_gen.region_stats()
    }

    /// Return the per-block old-generation statistics view.
    /// Aliases [`Heap::old_region_stats`]; both methods read the
    /// same `block_region_stats` source. The reported
    /// `hole_bytes` reflect the *physical* layout of the heap:
    /// they only shrink when bytes are actually moved (via
    /// physical compaction).
    pub fn old_block_region_stats(&self) -> Vec<OldRegionStats> {
        self.old_gen.block_region_stats()
    }

    /// Return the currently selected old-block compaction
    /// candidates from the block-side selector. Returned
    /// `region_index` fields refer to block indices in the
    /// per-block view.
    ///
    /// The selector runs the same heuristic the major-cycle
    /// planner uses: `hole_bytes >= selective_reclaim_threshold_bytes`,
    /// ranked by compaction efficiency, capped at
    /// `compaction_candidate_limit` and
    /// `max_compaction_bytes_per_cycle`.
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

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. Any finalizers beyond `max` stay queued
    /// for the next drain call.
    ///
    /// `max == 0` returns immediately with `0`.
    ///
    /// Intended for VM-driven cooperative finalization: the host
    /// runtime can run a fixed budget of finalizers per scheduler
    /// tick without committing to draining the entire queue at
    /// once.
    pub fn drain_pending_finalizers_bounded(&self, max: usize) -> u64 {
        self.runtime_state.drain_pending_finalizers_bounded(max)
    }

    /// Number of remembered old-to-young owners currently
    /// tracked by the explicit-edge fallback path. Each owner
    /// represents at least one (deduped) old-to-young edge into
    /// the nursery; the dense per-edge view was retired in
    /// favor of owner-only tracking.
    ///
    /// The unified `HeapStats::remembered_edges` and
    /// `remembered_owners` counters fold this with the per-block
    /// dirty card count. The split
    /// `HeapStats::remembered_explicit_*` counters report this
    /// path in isolation.
    pub fn remembered_edge_count(&self) -> usize {
        // `effective_len` folds any hot-path inserts sitting in
        // `pending_inserts` into a deduped count without
        // allocating when pending is empty, so external
        // observers see the same number GC-time consumers will
        // see after `merge_pending_owners`.
        self.indexes.remembered.effective_len()
    }

    #[cfg(test)]
    pub(crate) fn remembered_owner_count(&self) -> usize {
        self.indexes.remembered.effective_len()
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

    /// Total number of pending old-to-young roots, summed across
    /// both the explicit-edge fallback `RememberedSetState`
    /// (used for non-block-backed owners) and the per-block
    /// dirty-card fast path. This is the unified view exposed
    /// to observers; the split contributions are also available
    /// via `HeapStats::remembered_explicit_edges` /
    /// `remembered_dirty_cards`.
    pub fn total_remembered_count(&self) -> usize {
        self.remembered_edge_count().saturating_add(self.dirty_card_count())
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
        self.barrier_stats.snapshot()
    }

    /// Reset cumulative barrier traffic counters back to zero.
    /// Recent diagnostic events retained in the bounded ring
    /// buffer are left untouched. Takes `&self` because the
    /// atomic counters can be reset without exclusive access.
    pub fn clear_barrier_stats(&self) {
        self.barrier_stats.clear();
    }

    /// Increment the heap-wide cumulative barrier counters
    /// for `kind`. Takes `&self` because
    /// [`crate::stats::AtomicBarrierStats`] uses relaxed
    /// atomic fetch-adds — the barrier hot path never needs
    /// exclusive access to the heap for this bookkeeping.
    /// The per-mutator diagnostic ring lives on
    /// [`crate::mutator::MutatorLocal`]; the collector
    /// pushes events there via
    /// [`crate::mutator::MutatorLocal::push_barrier_event`]
    /// during the same barrier hook that bumps the stats
    /// here.
    pub(crate) fn bump_barrier_stats(&self, kind: BarrierKind) {
        match kind {
            BarrierKind::PostWrite => self.barrier_stats.bump_post_write(),
            BarrierKind::SatbPreWrite => self.barrier_stats.bump_satb_pre_write(),
        }
    }

    /// Barrier hot-path entry. Takes `&self` so `store_edge` /
    /// `post_write_barrier` can run under a `HeapCore` read
    /// lock. The fallback set mutation is routed through
    /// [`RememberedSetState::record_owner_shared`], which only
    /// needs its own per-set mutex.
    pub(crate) fn record_remembered_edge_if_needed(
        &self,
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

/// Drain pending finalizers at the controlled boundary of `HeapCore`
/// drop so that any arena- or old-block-backed `ObjectRecord`s
/// sitting in `RuntimeState::pending_finalizers` run their payload
/// `drop_in_place` while the backing buffers in `NurseryState` /
/// `OldGenState` are still alive.
///
/// Without this, a `SharedHeap` clone of `RuntimeStateHandle` can keep
/// the `RuntimeState` alive past `HeapCore`'s drop. When that Arc
/// finally hits zero, the pending `ObjectRecord`s try to deref
/// headers in arena or old-block buffers that have already been
/// freed as part of `HeapCore`'s field-order drop sequence.
///
/// The Drop lives on `HeapCore` rather than the outer `Heap` wrapper
/// so the drain runs whether the `HeapCore` is owned directly (rare,
/// test-only) or held behind the `Heap` newtype (the public entry).
impl Drop for HeapCore {
    fn drop(&mut self) {
        let _ = self.runtime_state.drain_pending_finalizers();
    }
}
