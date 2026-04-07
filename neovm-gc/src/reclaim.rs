use crate::heap::AllocError;
use crate::index_state::{ForwardingMap, HeapIndexState, PreparedIndexReclaim};
use crate::object::{ObjectRecord, OldBlockPlacement, OldRegionPlacement, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{
    OldBlock, OldGenConfig, OldGenState, OldRegionCollectionStats, PreparedOldGenReclaim,
};
use crate::stats::{CollectionStats, HeapStats, PreparedHeapStats};

/// Physical old-gen compaction helper (physical-compaction step 3).
///
/// Walks `objects` and computes the sum of `total_size` for every
/// live block-backed object, grouped by `OldBlockPlacement::block_index`.
/// Only objects that a previous pass has identified as survivors
/// (i.e. still present in the slice after mark processing) are
/// counted. The returned vector has one entry per block in
/// `blocks`; entries for blocks that hold no surviving records
/// stay at zero.
#[allow(dead_code)]
pub(crate) fn compute_per_block_live_bytes(
    objects: &[ObjectRecord],
    block_count: usize,
) -> Vec<usize> {
    let mut live_by_block = vec![0usize; block_count];
    for object in objects {
        if object.space() != SpaceKind::Old {
            continue;
        }
        let Some(placement) = object.old_block_placement() else {
            continue;
        };
        if let Some(slot) = live_by_block.get_mut(placement.block_index) {
            *slot = slot.saturating_add(object.total_size());
        }
    }
    live_by_block
}

/// Physical old-gen compaction helper (physical-compaction step 3).
///
/// Identify the indices of blocks whose current live-byte density
/// falls at or below `density_threshold` relative to their
/// capacity. These are the candidates the compaction pass will
/// evacuate from: copying their survivors into fresh target
/// blocks leaves the source blocks empty so the existing
/// block-reclaim path can drop them.
///
/// `density_threshold` is in the range `[0.0, 1.0]`. A value of
/// `0.3` means "blocks with 30% or less live fill are candidates."
/// Blocks that are empty are excluded (nothing to evacuate).
///
/// The returned vec is sorted by *ascending density*: the
/// emptiest blocks come first. This gives the compaction loop
/// the best-bang-for-buck ordering — moving a single survivor
/// out of a 1%-full block reclaims more space than moving it
/// out of a 50%-full block, and the compaction target packing
/// works best when we evacuate the most-wasted blocks first.
#[allow(dead_code)]
pub(crate) fn find_sparse_old_block_candidates(
    live_by_block: &[usize],
    blocks: &[OldBlock],
    density_threshold: f64,
) -> Vec<usize> {
    let mut candidates: Vec<(usize, f64)> = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let live = live_by_block.get(index).copied().unwrap_or(0);
        if live == 0 {
            continue; // empty blocks get dropped by drop_unused_blocks_with_remap.
        }
        let capacity = block.capacity_bytes();
        if capacity == 0 {
            continue;
        }
        let density = (live as f64) / (capacity as f64);
        if density <= density_threshold {
            candidates.push((index, density));
        }
    }
    // Sort by ascending density so the emptiest blocks come
    // first. Density values come from (live_bytes / capacity)
    // and are bounded in [0.0, 1.0]; partial_cmp is safe here
    // because the inputs are real, finite, non-NaN.
    candidates.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.into_iter().map(|(index, _)| index).collect()
}

/// Physical old-gen compaction pass (physical-compaction step 4).
///
/// Walks `objects`, identifies sparse OldBlock candidates whose
/// live density is at or below `density_threshold`, evacuates
/// every surviving record in those blocks into freshly-created
/// target blocks via [`evacuate_old_object_to_fresh_block`], and
/// replaces the source records in `objects` with the evacuated
/// ones. Returns a [`ForwardingMap`] of
/// `(old_object_key, new_GcErased)` entries that a subsequent
/// relocation pass can feed through [`ForwardingRelocator`] to
/// rewrite any inbound reference.
///
/// This function does NOT touch the source block contents: the
/// source blocks become empty (no surviving records point into
/// them), and the existing
/// [`rebuild_line_marks_and_reclaim_empty_old_blocks`] pass drops
/// them as part of the post-sweep rebuild.
///
/// If no blocks qualify as sparse, the function returns an empty
/// forwarding map and leaves `objects` untouched. Allocation
/// failures during evacuation are treated per-object: the
/// failing record is left in place, the forwarding map omits it,
/// and the pass moves on. Callers get a best-effort compaction.
#[allow(dead_code)]
pub(crate) fn compact_sparse_old_blocks(
    objects: &mut [ObjectRecord],
    old_gen: &mut OldGenState,
    config: &OldGenConfig,
    density_threshold: f64,
) -> ForwardingMap {
    let mut forwarding = ForwardingMap::new();
    if objects.is_empty() || old_gen.block_count() == 0 {
        return forwarding;
    }

    // Phase A: compute per-block live_bytes from the post-mark
    // object slice and pick sparse candidates.
    let live_by_block = compute_per_block_live_bytes(objects, old_gen.block_count());
    let candidates = find_sparse_old_block_candidates(
        &live_by_block,
        old_gen.blocks(),
        density_threshold,
    );
    if candidates.is_empty() {
        return forwarding;
    }
    let candidate_set: std::collections::HashSet<usize> = candidates.into_iter().collect();

    // Phase B: walk every record; for each one whose block is a
    // candidate, evacuate it into a compaction target block and
    // swap the record in place. Multiple survivors share the
    // same target block until it fills up, at which point
    // alloc_for_compaction_into_target rolls over to a fresh
    // one. This packs survivors tight instead of creating one
    // new block per evacuated record.
    let mut target_hint: Option<usize> = None;
    let total = objects.len();
    #[allow(clippy::needless_range_loop)]
    for slot_index in 0..total {
        let (is_candidate, object_key) = {
            let object = &objects[slot_index];
            if object.space() != SpaceKind::Old {
                (false, None)
            } else {
                match object.old_block_placement() {
                    Some(placement) if candidate_set.contains(&placement.block_index) => {
                        (true, Some(object.object_key()))
                    }
                    _ => (false, None),
                }
            }
        };
        if !is_candidate {
            continue;
        }
        let Some(object_key) = object_key else {
            continue;
        };
        // Derive the allocation layout from the source record.
        let layout = {
            let source = &objects[slot_index];
            match core::alloc::Layout::from_size_align(
                source.total_size(),
                source.layout_align(),
            ) {
                Ok(l) => l,
                Err(_) => continue,
            }
        };
        // Allocate the target slot via the compaction-aware
        // allocator: prefer the current target block, fall
        // forward to a fresh block when the current one is full.
        let Some((placement, base, new_target)) =
            old_gen.alloc_for_compaction_into_target(config, layout, target_hint)
        else {
            continue;
        };
        target_hint = Some(new_target);
        // Copy the payload and install the forwarding pointer.
        let evacuated = {
            let source = &objects[slot_index];
            // SAFETY: `alloc_for_compaction_into_target` returned
            // a freshly-reserved slot in a non-source block
            // backed by a buffer owned by the pool. The layout
            // matches what evacuate_to_arena_slot expects.
            let mut record =
                match unsafe { source.evacuate_to_arena_slot(SpaceKind::Old, base) } {
                    Ok(r) => r,
                    Err(_) => continue,
                };
            record.set_old_block_placement(placement);
            record
        };
        let new_erased = evacuated.erased();
        // Replace the source record in place with the evacuated
        // one. The source block's bytes remain in the pool buffer
        // but no record now references them; the post-compact
        // rebuild will drop the source block because none of
        // its lines are marked any more.
        objects[slot_index] = evacuated;
        forwarding.insert(object_key, new_erased);
    }

    forwarding
}

/// Physical old-gen compaction helper (physical-compaction step 2).
///
/// Copies the payload of `source` into a freshly-created OldBlock
/// target slot and returns a new `ObjectRecord` owning that slot.
/// `evacuate_to_arena_slot` installs a forwarding pointer on the
/// source header as a side effect, so a later relocation pass can
/// rewrite every inbound reference.
///
/// The target slot is allocated via
/// [`OldGenState::alloc_in_fresh_block`] to guarantee the target
/// block is NOT one of the sparse source blocks we are evacuating
/// from — a fresh block had zero live bytes before this call, so it
/// cannot collide with any source placement.
///
/// Returns `Err(AllocError)` if the target allocation or the
/// payload layout cannot be satisfied. Callers should treat such
/// failures as "skip this evacuation, leave the source record in
/// place" rather than aborting the whole reclaim cycle.
#[allow(dead_code)]
pub(crate) fn evacuate_old_object_to_fresh_block(
    old_gen: &mut OldGenState,
    config: &OldGenConfig,
    source: &ObjectRecord,
) -> Result<ObjectRecord, AllocError> {
    let total_size = source.total_size();
    let align = source.layout_align();
    let layout = core::alloc::Layout::from_size_align(total_size, align)
        .map_err(|_| AllocError::LayoutOverflow)?;
    let (placement, base) = old_gen
        .alloc_in_fresh_block(config, layout)
        .ok_or(AllocError::LayoutOverflow)?;
    // SAFETY: `alloc_in_fresh_block` returned a freshly-created
    // slot in a brand-new OldBlock whose backing buffer outlives
    // the pool. The layout matches what evacuate_to_arena_slot
    // expects. The source record's payload remains live until we
    // replace it in the objects vec.
    let mut evacuated = unsafe { source.evacuate_to_arena_slot(SpaceKind::Old, base)? };
    evacuated.set_old_block_placement(placement);
    Ok(evacuated)
}

#[derive(Debug)]
pub(crate) struct PreparedReclaimSurvivor {
    /// Original index in `Heap::objects` before reclaim commit.
    pub(crate) object_index: usize,
    pub(crate) old_region_placement: Option<OldRegionPlacement>,
}

#[derive(Debug)]
pub(crate) struct PreparedReclaim {
    pub(crate) promoted_bytes: usize,
    pub(crate) old_gen: PreparedOldGenReclaim,
    /// Per-subsystem reclaim state assembled under `HeapIndexState`.
    /// This is the single source of truth for finalize_indices and the
    /// rebuilt candidate lists — `commit_prepared_reclaim_objects` reads
    /// `indexes.finalize_indices` directly rather than duplicating it at
    /// the top level.
    pub(crate) indexes: PreparedIndexReclaim,
    /// Survivors in ascending original `object_index` order.
    ///
    /// `commit_prepared_reclaim_objects` drains this in lockstep with the original
    /// `objects` vector, so ordering is part of the prepared-state contract.
    pub(crate) survivors: Vec<PreparedReclaimSurvivor>,
    pub(crate) stats: PreparedHeapStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MinorRebuildResult {
    pub(crate) queued_finalizers: u64,
    pub(crate) old_region_stats: OldRegionCollectionStats,
    pub(crate) after_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReclaimCommitResult {
    pub(crate) queued_finalizers: u64,
    pub(crate) old_region_stats: OldRegionCollectionStats,
    pub(crate) after_bytes: usize,
}

pub(crate) fn prepare_reclaim(
    objects: &[ObjectRecord],
    indexes: &HeapIndexState,
    old_gen: &OldGenState,
    old_config: &OldGenConfig,
    kind: CollectionKind,
    plan: &CollectionPlan,
) -> PreparedReclaim {
    let mut rebuild = old_gen.prepare_rebuild_for_plan(plan);
    let mut survivors = Vec::new();
    let mut prepared_stats = PreparedHeapStats::default();

    for (object_index, object) in objects.iter().enumerate() {
        if !keep_object_for_collection(kind, object) {
            continue;
        }

        let total_size = object.total_size();

        let old_region_placement = match object.space() {
            SpaceKind::Old => OldGenState::prepare_reclaim_survivor(
                &mut rebuild,
                old_config,
                object
                    .old_region_placement()
                    .expect("live old object should retain old-region placement"),
                total_size,
            ),
            _ => None,
        };
        survivors.push(PreparedReclaimSurvivor {
            object_index,
            old_region_placement,
        });
        prepared_stats.record_live_object(object.space(), total_size);
    }

    let prepared_old_gen = OldGenState::finish_prepared_rebuild(rebuild, &mut survivors);
    let prepared_indexes = indexes.prepare_reclaim_state(objects, &survivors, kind);
    PreparedReclaim {
        promoted_bytes: 0,
        old_gen: prepared_old_gen,
        indexes: prepared_indexes,
        survivors,
        stats: prepared_stats,
    }
}

pub(crate) fn prepare_major_reclaim(
    plan: &CollectionPlan,
    process_weak_references: impl FnOnce(&CollectionPlan),
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> PreparedReclaim,
) -> PreparedReclaim {
    process_weak_references(plan);
    prepare_reclaim(plan)
}

pub(crate) fn prepare_full_reclaim<Heap, Forwarding>(
    heap: &mut Heap,
    plan: &CollectionPlan,
    evacuate_marked_nursery: impl FnOnce(&mut Heap) -> Result<(Forwarding, usize), AllocError>,
    relocate_roots_and_edges: impl FnOnce(&mut Heap, &Forwarding),
    process_weak_references: impl FnOnce(&mut Heap, &CollectionPlan, &Forwarding),
    prepare_reclaim: impl FnOnce(&Heap, &CollectionPlan) -> PreparedReclaim,
) -> Result<PreparedReclaim, AllocError> {
    let (forwarding, promoted_bytes) = evacuate_marked_nursery(heap)?;
    relocate_roots_and_edges(heap, &forwarding);
    process_weak_references(heap, plan, &forwarding);
    Ok(PreparedReclaim {
        promoted_bytes,
        ..prepare_reclaim(heap, plan)
    })
}

pub(crate) fn commit_prepared_reclaim_objects(
    old_objects: Vec<ObjectRecord>,
    prepared_reclaim: &PreparedReclaim,
    mut enqueue_pending_finalizer: impl FnMut(ObjectRecord) -> u64,
) -> (Vec<ObjectRecord>, u64) {
    debug_assert!(
        prepared_reclaim
            .survivors
            .windows(2)
            .all(|window| window[0].object_index < window[1].object_index),
        "prepared reclaim survivors must stay sorted by original object index"
    );
    debug_assert!(
        prepared_reclaim
            .indexes
            .finalize_indices
            .windows(2)
            .all(|window| window[0] < window[1]),
        "prepared reclaim finalizer indices must stay sorted by original object index"
    );

    let mut queued_finalizers = 0u64;
    let mut survivor_iter = prepared_reclaim.survivors.iter().peekable();
    let mut finalize_iter = prepared_reclaim
        .indexes
        .finalize_indices
        .iter()
        .copied()
        .peekable();
    let mut object_index = 0usize;
    let mut rebuilt_objects = Vec::with_capacity(old_objects.len());

    // Prepared reclaim is assembled in original object order. Finish drains
    // that prepared order in lockstep with the owned `objects` vector so
    // commit stays linear while dead finalizable objects are transferred to
    // the pending-finalizer queue instead of running inline during GC.
    for mut object in old_objects {
        let current_index = object_index;
        object_index = object_index.saturating_add(1);
        let should_finalize = finalize_iter
            .peek()
            .is_some_and(|&pending_index| pending_index == current_index);
        if should_finalize {
            finalize_iter.next();
            queued_finalizers = queued_finalizers.saturating_add(enqueue_pending_finalizer(object));
            continue;
        }

        let Some(survivor) =
            survivor_iter.next_if(|survivor| survivor.object_index == current_index)
        else {
            continue;
        };

        object.clear_mark();
        if let Some(placement) = survivor.old_region_placement {
            object.set_old_region_placement(placement);
        }
        rebuilt_objects.push(object);
    }

    debug_assert!(
        survivor_iter.next().is_none(),
        "prepared reclaim survivors should all be drained during finish"
    );
    debug_assert!(
        finalize_iter.next().is_none(),
        "prepared reclaim finalizers should all be drained during finish"
    );

    (rebuilt_objects, queued_finalizers)
}

/// Rebuild old-block line marks from currently surviving records and pending
/// finalizers, then drop blocks whose line marks are entirely empty. Surviving
/// records' `OldBlockPlacement::block_index` values are rebound through the
/// new block index map after empty blocks are dropped.
pub(crate) fn rebuild_line_marks_and_reclaim_empty_old_blocks(
    objects: &mut [ObjectRecord],
    old_gen: &mut OldGenState,
    runtime_state: &RuntimeStateHandle,
) -> usize {
    // Snapshot pending finalizer placements first so blocks they pin stay
    // marked even though their owning records are no longer in `objects`.
    let pending_placements = runtime_state.snapshot_pending_finalizer_block_placements();
    old_gen.clear_all_block_line_marks();
    // Phase 4 perf: also rebuild the per-card object-start index from
    // surviving block-backed records so the next minor cycle's dirty-card
    // root scan can iterate dirty cards in O(dirty_cards) instead of doing
    // a linear pass over every record per dirty card.
    old_gen.clear_all_block_object_starts();
    // OldRegion unification step 9: also reset per-block
    // live_bytes / object_count / occupied_lines so the survivor
    // walk below can re-populate them. Without this the counters
    // stay at their pre-sweep monotonic values and over-report
    // live bytes.
    old_gen.clear_all_block_live_accounting();
    for object in objects.iter() {
        if let Some(placement) = object.old_block_placement() {
            old_gen.mark_block_lines_for_placement(placement);
            old_gen.record_block_object_start_for_placement(placement);
            old_gen.record_block_object_accounting_for_placement(placement);
        }
    }
    for placement in &pending_placements {
        old_gen.mark_block_lines_for_placement(*placement);
        old_gen.record_block_object_start_for_placement(*placement);
        old_gen.record_block_object_accounting_for_placement(*placement);
    }

    let remap = old_gen.drop_unused_blocks_with_remap();
    let dropped = remap.iter().filter(|entry| entry.is_none()).count();
    if dropped == 0 {
        return 0;
    }

    // Apply remap to surviving records.
    for object in objects.iter_mut() {
        let Some(placement) = object.old_block_placement() else {
            continue;
        };
        let Some(&Some(new_index)) = remap.get(placement.block_index) else {
            // Should not happen for live records, but stay defensive.
            continue;
        };
        if new_index != placement.block_index {
            object.set_old_block_placement(OldBlockPlacement {
                block_index: new_index,
                ..placement
            });
        }
    }
    runtime_state.rebind_pending_finalizer_block_indices(&remap);
    dropped
}

pub(crate) fn apply_prepared_reclaim(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    stats: &mut HeapStats,
    runtime_state: &RuntimeStateHandle,
    prepared_reclaim: PreparedReclaim,
    enqueue_pending_finalizer: impl FnMut(ObjectRecord) -> u64,
) -> ReclaimCommitResult {
    let old_objects = core::mem::take(objects);
    let (rebuilt_objects, queued_finalizers) =
        commit_prepared_reclaim_objects(old_objects, &prepared_reclaim, enqueue_pending_finalizer);

    let PreparedReclaim {
        old_gen: prepared_old_gen,
        indexes: prepared_indexes,
        stats: prepared_stats,
        ..
    } = prepared_reclaim;
    *objects = rebuilt_objects;
    let old_region_stats = old_gen.apply_prepared_reclaim(prepared_old_gen);
    indexes.apply_prepared_reclaim(prepared_indexes);
    // Rebuild line marks across surviving block-backed records (and any
    // pending finalizer placements that still pin a block), then drop
    // every block whose lines remain entirely free.
    rebuild_line_marks_and_reclaim_empty_old_blocks(objects, old_gen, runtime_state);
    let old_reserved_bytes = old_gen.reserved_bytes();
    let after_bytes = prepared_stats.apply_space_rebuild(stats, old_reserved_bytes);
    ReclaimCommitResult {
        queued_finalizers,
        old_region_stats,
        after_bytes,
    }
}

pub(crate) fn finish_prepared_reclaim_cycle(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    stats: &mut HeapStats,
    runtime_state: &RuntimeStateHandle,
    before_bytes: usize,
    mark_steps: u64,
    mark_rounds: u64,
    reclaim_prepare_nanos: u64,
    prepared_reclaim: PreparedReclaim,
    enqueue_pending_finalizer: impl FnMut(ObjectRecord) -> u64,
) -> CollectionStats {
    let promoted_bytes = prepared_reclaim.promoted_bytes;
    let commit = apply_prepared_reclaim(
        objects,
        indexes,
        old_gen,
        stats,
        runtime_state,
        prepared_reclaim,
        enqueue_pending_finalizer,
    );
    CollectionStats::completed_old_gen_cycle(
        mark_steps,
        mark_rounds,
        promoted_bytes,
        reclaim_prepare_nanos,
        before_bytes,
        commit.after_bytes,
        commit.queued_finalizers,
        commit.old_region_stats,
    )
}

pub(crate) fn sweep_minor_and_rebuild_post_collection(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    stats: &mut HeapStats,
    runtime_state: &RuntimeStateHandle,
    kind: CollectionKind,
    completed_plan: Option<CollectionPlan>,
    mut enqueue_pending_finalizer: impl FnMut(ObjectRecord) -> u64,
) -> MinorRebuildResult {
    let old_objects = core::mem::take(objects);
    let mut old_region_rebuild = old_gen.prepare_rebuild(completed_plan.as_ref());
    let post_sweep_indexes = indexes.begin_post_sweep_rebuild(old_objects.len());
    let mut rebuilt_stats = PreparedHeapStats::default();

    let mut rebuilt_objects = Vec::with_capacity(old_objects.len());
    let mut queued_finalizers = 0u64;
    for mut object in old_objects {
        if !keep_object_for_collection(kind, &object) {
            if post_sweep_indexes.should_enqueue_finalizer(&object) {
                queued_finalizers =
                    queued_finalizers.saturating_add(enqueue_pending_finalizer(object));
            }
            continue;
        }

        object.clear_mark();
        let object_key = object.object_key();
        let desc = object.header().desc();
        let space = object.space();
        let total_size = object.total_size();
        if space == SpaceKind::Old {
            OldGenState::rebuild_post_sweep_object(
                old_config,
                &mut object,
                total_size,
                old_region_rebuild.as_mut(),
            );
        }
        let index = rebuilt_objects.len();
        rebuilt_objects.push(object);
        indexes.record_allocated_object(object_key, index, desc);
        rebuilt_stats.record_live_object(space, total_size);
    }

    *objects = rebuilt_objects;
    let (rebuilt_old_regions, old_region_stats) =
        OldGenState::finish_rebuild(old_region_rebuild, objects);
    if let Some(rebuilt_old_regions) = rebuilt_old_regions {
        old_gen.regions = rebuilt_old_regions;
    }
    // Rebuild block-level line marks from surviving records (and pending
    // finalizers) and reclaim any block whose lines remain entirely free.
    rebuild_line_marks_and_reclaim_empty_old_blocks(objects, old_gen, runtime_state);
    let after_bytes = rebuilt_stats.apply_space_rebuild(stats, old_gen.reserved_bytes());
    indexes.retain_remembered_edges_for_post_sweep_objects(objects);
    MinorRebuildResult {
        queued_finalizers,
        old_region_stats,
        after_bytes,
    }
}

fn keep_object_for_collection(kind: CollectionKind, object: &ObjectRecord) -> bool {
    match kind {
        CollectionKind::Minor => {
            object.space() == SpaceKind::Immortal
                || object.space() != SpaceKind::Nursery
                || (object.is_marked() && !object.header().is_moved_out())
        }
        CollectionKind::Major | CollectionKind::Full => {
            object.space() == SpaceKind::Immortal
                || (object.is_marked() && !object.header().is_moved_out())
        }
    }
}

#[cfg(test)]
#[path = "reclaim_test.rs"]
mod tests;
