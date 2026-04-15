use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use parking_lot::Mutex;

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
        let nursery_survivor_bytes =
            (nursery_bytes_after as u64).saturating_add(promoted_bytes as u64);
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
///
/// The snapshot struct is plain `u64` so callers can compare
/// and copy it cheaply. The live counters inside the heap
/// live on [`AtomicBarrierStats`] for lock-free updates from
/// the mutator barrier path.
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

/// Atomic counterpart of [`BarrierStats`] held inside the
/// crate-internal heap core. The barrier hook bumps these
/// counters through mutator-owned counter slots, avoiding the
/// heap write lock and shared RMWs on the barrier hot path.
/// Observers read a [`BarrierStats`] snapshot via
/// [`AtomicBarrierStats::snapshot`].
#[derive(Debug, Default)]
#[repr(align(64))]
struct BarrierCounterShard {
    post_write: AtomicU64,
    satb_pre_write: AtomicU64,
}

#[derive(Debug, Default)]
pub(crate) struct BarrierStatsLocal {
    slot_index: usize,
    slot: Option<Arc<BarrierCounterShard>>,
    generation: usize,
    post_write: u64,
    satb_pre_write: u64,
}

impl BarrierStatsLocal {
    #[cfg(test)]
    pub(crate) fn is_registered(&self) -> bool {
        self.slot.is_some()
    }

    fn refresh_from_slot(&mut self, generation: usize) {
        let Some(slot) = self.slot.as_ref() else {
            return;
        };
        self.generation = generation;
        self.post_write = slot.post_write.load(Ordering::Relaxed);
        self.satb_pre_write = slot.satb_pre_write.load(Ordering::Relaxed);
    }
}

/// Shared heap-wide barrier counter registry. Each mutator
/// owns one slot and publishes its cumulative barrier totals
/// with relaxed stores; snapshots sum every live slot.
#[derive(Debug)]
pub struct AtomicBarrierStats {
    slots: Mutex<Vec<Arc<BarrierCounterShard>>>,
    free_slots: Mutex<Vec<usize>>,
    generation: AtomicUsize,
}

impl Default for AtomicBarrierStats {
    fn default() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            free_slots: Mutex::new(Vec::new()),
            generation: AtomicUsize::new(0),
        }
    }
}

impl AtomicBarrierStats {
    #[inline]
    fn generation(&self) -> usize {
        self.generation.load(Ordering::Acquire)
    }

    /// Construct a fresh set of counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_local(&self) -> BarrierStatsLocal {
        if let Some(slot_index) = self.free_slots.lock().pop() {
            let slot = {
                let slots = self.slots.lock();
                Arc::clone(
                    slots
                        .get(slot_index)
                        .expect("released barrier stats slot should exist"),
                )
            };
            let mut local = BarrierStatsLocal {
                slot_index,
                slot: Some(slot),
                ..BarrierStatsLocal::default()
            };
            local.refresh_from_slot(self.generation());
            return local;
        }

        let slot = Arc::new(BarrierCounterShard::default());
        let slot_index = {
            let mut slots = self.slots.lock();
            let slot_index = slots.len();
            slots.push(Arc::clone(&slot));
            slot_index
        };
        let mut local = BarrierStatsLocal {
            slot_index,
            slot: Some(slot),
            ..BarrierStatsLocal::default()
        };
        local.refresh_from_slot(self.generation());
        local
    }

    pub(crate) fn release_local(&self, local: &mut BarrierStatsLocal) {
        if local.slot.take().is_some() {
            self.free_slots.lock().push(local.slot_index);
            local.generation = 0;
        }
    }

    /// Bump the post-write counter by one.
    #[inline(always)]
    pub(crate) fn bump_post_write(&self, local: &mut BarrierStatsLocal) {
        self.refresh_local(local);
        local.post_write = local.post_write.saturating_add(1);
        local
            .slot
            .as_deref()
            .expect("barrier stats local should be registered")
            .post_write
            .store(local.post_write, Ordering::Relaxed);
    }

    /// Bump the SATB pre-write counter by one.
    #[inline(always)]
    pub(crate) fn bump_satb_pre_write(&self, local: &mut BarrierStatsLocal) {
        self.refresh_local(local);
        local.satb_pre_write = local.satb_pre_write.saturating_add(1);
        local
            .slot
            .as_deref()
            .expect("barrier stats local should be registered")
            .satb_pre_write
            .store(local.satb_pre_write, Ordering::Relaxed);
    }

    /// Return a consistent snapshot of the current counter values.
    pub fn snapshot(&self) -> BarrierStats {
        let slots = self.slots.lock();
        BarrierStats {
            post_write: slots
                .iter()
                .map(|slot| slot.post_write.load(Ordering::Relaxed))
                .sum(),
            satb_pre_write: slots
                .iter()
                .map(|slot| slot.satb_pre_write.load(Ordering::Relaxed))
                .sum(),
        }
    }

    /// Reset every counter to zero.
    pub fn clear(&self) {
        let slots = self.slots.lock();
        for slot in slots.iter() {
            slot.post_write.store(0, Ordering::Relaxed);
            slot.satb_pre_write.store(0, Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    #[inline(always)]
    fn refresh_local(&self, local: &mut BarrierStatsLocal) {
        let generation = self.generation();
        if local.generation != generation {
            local.refresh_from_slot(generation);
        }
    }
}

/// Atomic counterpart of the per-space `live_bytes` and
/// `reserved_bytes` fields in [`HeapStats`]. The allocation
/// hot path bumps these counters with plain `Relaxed`
/// atomic adds so it does not need exclusive access to
/// `HeapStats`. Observers read a consistent snapshot
/// overlaid onto a `HeapStats` via
/// [`AtomicAllocationCounters::apply_to`].
///
/// The counters are authoritative for the five
/// `{nursery,old,pinned,large,immortal}.live_bytes` fields
/// and the three `{old,large,immortal}.reserved_bytes`
/// fields. GC-time paths that rewrite these counters
/// (e.g. [`PreparedHeapStats::apply_space_rebuild`]) must
/// also update these atomics via [`Self::sync_from`] so
/// the hot-path readers see the post-cycle values.
#[derive(Debug, Default)]
#[repr(align(64))]
struct AllocationCounterShard {
    nursery_live_bytes: AtomicUsize,
    old_live_bytes: AtomicUsize,
    pinned_live_bytes: AtomicUsize,
    large_live_bytes: AtomicUsize,
    large_reserved_bytes: AtomicUsize,
    immortal_live_bytes: AtomicUsize,
    immortal_reserved_bytes: AtomicUsize,
}

#[derive(Debug, Default)]
pub(crate) struct AllocationCounterLocal {
    slot_index: usize,
    slot: Option<Arc<AllocationCounterShard>>,
    generation: usize,
    nursery_live_bytes: usize,
    old_live_bytes: usize,
    pinned_live_bytes: usize,
    large_live_bytes: usize,
    large_reserved_bytes: usize,
    immortal_live_bytes: usize,
    immortal_reserved_bytes: usize,
}

impl AllocationCounterLocal {
    #[cfg(test)]
    pub(crate) fn is_registered(&self) -> bool {
        self.slot.is_some()
    }

    fn refresh_from_slot(&mut self, generation: usize) {
        let Some(slot) = self.slot.as_ref() else {
            return;
        };
        self.generation = generation;
        self.nursery_live_bytes = slot.nursery_live_bytes.load(Ordering::Relaxed);
        self.old_live_bytes = slot.old_live_bytes.load(Ordering::Relaxed);
        self.pinned_live_bytes = slot.pinned_live_bytes.load(Ordering::Relaxed);
        self.large_live_bytes = slot.large_live_bytes.load(Ordering::Relaxed);
        self.large_reserved_bytes = slot.large_reserved_bytes.load(Ordering::Relaxed);
        self.immortal_live_bytes = slot.immortal_live_bytes.load(Ordering::Relaxed);
        self.immortal_reserved_bytes = slot.immortal_reserved_bytes.load(Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(crate) struct AtomicAllocationCounters {
    slots: Mutex<Vec<Arc<AllocationCounterShard>>>,
    free_slots: Mutex<Vec<usize>>,
    old_reserved_bytes: AtomicUsize,
    generation: AtomicUsize,
}

impl Default for AtomicAllocationCounters {
    fn default() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            free_slots: Mutex::new(Vec::new()),
            old_reserved_bytes: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
        }
    }
}

impl AtomicAllocationCounters {
    #[inline]
    fn generation(&self) -> usize {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn register_local(&self) -> AllocationCounterLocal {
        if let Some(slot_index) = self.free_slots.lock().pop() {
            let slot = {
                let slots = self.slots.lock();
                Arc::clone(
                    slots
                        .get(slot_index)
                        .expect("released allocation counter slot should exist"),
                )
            };
            let mut local = AllocationCounterLocal {
                slot_index,
                slot: Some(slot),
                ..AllocationCounterLocal::default()
            };
            local.refresh_from_slot(self.generation());
            return local;
        }

        let slot = Arc::new(AllocationCounterShard::default());
        let slot_index = {
            let mut slots = self.slots.lock();
            let slot_index = slots.len();
            slots.push(Arc::clone(&slot));
            slot_index
        };
        let mut local = AllocationCounterLocal {
            slot_index,
            slot: Some(slot),
            ..AllocationCounterLocal::default()
        };
        local.refresh_from_slot(self.generation());
        local
    }

    pub(crate) fn release_local(&self, local: &mut AllocationCounterLocal) {
        if local.slot.take().is_some() {
            self.free_slots.lock().push(local.slot_index);
            local.generation = 0;
        }
    }

    /// Record one allocation. Mirrors the logic of
    /// [`HeapStats::record_allocation`] but keeps a cached
    /// running total per mutator-owned slot and publishes the
    /// new total with relaxed stores instead of locked RMWs.
    pub(crate) fn record_allocation(
        &self,
        space: SpaceKind,
        bytes: usize,
        old_reserved_bytes: usize,
        local: &mut AllocationCounterLocal,
    ) {
        self.refresh_local(local);
        match space {
            SpaceKind::Nursery => {
                local.nursery_live_bytes = local.nursery_live_bytes.saturating_add(bytes);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .nursery_live_bytes
                    .store(local.nursery_live_bytes, Ordering::Relaxed);
            }
            SpaceKind::Old => {
                local.old_live_bytes = local.old_live_bytes.saturating_add(bytes);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .old_live_bytes
                    .store(local.old_live_bytes, Ordering::Relaxed);
                self.old_reserved_bytes
                    .store(old_reserved_bytes, Ordering::Relaxed);
            }
            SpaceKind::Pinned => {
                local.pinned_live_bytes = local.pinned_live_bytes.saturating_add(bytes);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .pinned_live_bytes
                    .store(local.pinned_live_bytes, Ordering::Relaxed);
            }
            SpaceKind::Large => {
                local.large_live_bytes = local.large_live_bytes.saturating_add(bytes);
                local.large_reserved_bytes = local.large_reserved_bytes.saturating_add(bytes);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .large_live_bytes
                    .store(local.large_live_bytes, Ordering::Relaxed);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .large_reserved_bytes
                    .store(local.large_reserved_bytes, Ordering::Relaxed);
            }
            SpaceKind::Immortal => {
                local.immortal_live_bytes = local.immortal_live_bytes.saturating_add(bytes);
                local.immortal_reserved_bytes = local.immortal_reserved_bytes.saturating_add(bytes);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .immortal_live_bytes
                    .store(local.immortal_live_bytes, Ordering::Relaxed);
                local
                    .slot
                    .as_deref()
                    .expect("allocation counter local should be registered")
                    .immortal_reserved_bytes
                    .store(local.immortal_reserved_bytes, Ordering::Relaxed);
            }
        }
    }

    #[inline(always)]
    pub(crate) fn record_nursery_allocation(
        &self,
        bytes: usize,
        local: &mut AllocationCounterLocal,
    ) {
        self.refresh_local(local);
        local.nursery_live_bytes = local.nursery_live_bytes.saturating_add(bytes);
        local
            .slot
            .as_deref()
            .expect("allocation counter local should be registered")
            .nursery_live_bytes
            .store(local.nursery_live_bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    fn refresh_local(&self, local: &mut AllocationCounterLocal) {
        let generation = self.generation();
        if local.generation != generation {
            local.refresh_from_slot(generation);
        }
    }

    /// Overlay the atomic counter values onto the given
    /// `HeapStats` snapshot. Called by
    /// [`crate::heap::HeapCore::storage_stats`] so that
    /// observers see the latest allocation counters without
    /// needing exclusive access.
    pub(crate) fn apply_to(&self, stats: &mut HeapStats) {
        let slots = self.slots.lock();
        stats.nursery.live_bytes = slots
            .iter()
            .map(|slot| slot.nursery_live_bytes.load(Ordering::Relaxed))
            .sum();
        stats.old.live_bytes = slots
            .iter()
            .map(|slot| slot.old_live_bytes.load(Ordering::Relaxed))
            .sum();
        stats.old.reserved_bytes = self.old_reserved_bytes.load(Ordering::Relaxed);
        stats.pinned.live_bytes = slots
            .iter()
            .map(|slot| slot.pinned_live_bytes.load(Ordering::Relaxed))
            .sum();
        stats.large.live_bytes = slots
            .iter()
            .map(|slot| slot.large_live_bytes.load(Ordering::Relaxed))
            .sum();
        stats.large.reserved_bytes = slots
            .iter()
            .map(|slot| slot.large_reserved_bytes.load(Ordering::Relaxed))
            .sum();
        stats.immortal.live_bytes = slots
            .iter()
            .map(|slot| slot.immortal_live_bytes.load(Ordering::Relaxed))
            .sum();
        stats.immortal.reserved_bytes = slots
            .iter()
            .map(|slot| slot.immortal_reserved_bytes.load(Ordering::Relaxed))
            .sum();
    }

    /// Synchronize the atomics from a `HeapStats` snapshot.
    /// Called by GC-time paths that rewrite the space
    /// counters (e.g. after `apply_space_rebuild`) so the
    /// hot-path atomic view stays in sync with the
    /// post-cycle ground truth.
    pub(crate) fn sync_from(&self, stats: &HeapStats) {
        let slots = self.slots.lock();
        for slot in slots.iter() {
            slot.nursery_live_bytes.store(0, Ordering::Relaxed);
            slot.old_live_bytes.store(0, Ordering::Relaxed);
            slot.pinned_live_bytes.store(0, Ordering::Relaxed);
            slot.large_live_bytes.store(0, Ordering::Relaxed);
            slot.large_reserved_bytes.store(0, Ordering::Relaxed);
            slot.immortal_live_bytes.store(0, Ordering::Relaxed);
            slot.immortal_reserved_bytes.store(0, Ordering::Relaxed);
        }
        if let Some(slot0) = slots.first() {
            slot0
                .nursery_live_bytes
                .store(stats.nursery.live_bytes, Ordering::Relaxed);
            slot0
                .old_live_bytes
                .store(stats.old.live_bytes, Ordering::Relaxed);
            slot0
                .pinned_live_bytes
                .store(stats.pinned.live_bytes, Ordering::Relaxed);
            slot0
                .large_live_bytes
                .store(stats.large.live_bytes, Ordering::Relaxed);
            slot0
                .large_reserved_bytes
                .store(stats.large.reserved_bytes, Ordering::Relaxed);
            slot0
                .immortal_live_bytes
                .store(stats.immortal.live_bytes, Ordering::Relaxed);
            slot0
                .immortal_reserved_bytes
                .store(stats.immortal.reserved_bytes, Ordering::Relaxed);
        }
        self.old_reserved_bytes
            .store(stats.old.reserved_bytes, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
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
    /// Number of remembered edges recorded via the
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
    /// Number of distinct old owners represented in the
    /// explicit-edge fallback path. Equal to the unique owner-
    /// set size of the owner-only fallback container.
    ///
    /// After the explicit-edge refactor, the fallback path
    /// stores deduped owners only (no per-edge entries), so
    /// `remembered_explicit_owners` and
    /// [`Self::remembered_explicit_edges`] always report the
    /// same number. Together with
    /// [`Self::remembered_dirty_card_owners`] they sum (modulo
    /// the dirty-card-as-owner approximation noted on the
    /// dirty-card counter) to the unified
    /// [`Self::remembered_owners`] view.
    pub remembered_explicit_owners: usize,
    /// Owner-side approximation for the per-block dirty-card
    /// fast path: equal to [`Self::remembered_dirty_cards`].
    /// Each dirty card represents at least one pending
    /// old-to-young root in its covered byte range, so the dirty
    /// card count is used as a conservative owner estimate when
    /// a precise per-card object identity is not yet tracked.
    pub remembered_dirty_card_owners: usize,
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

    #[allow(dead_code)]
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
