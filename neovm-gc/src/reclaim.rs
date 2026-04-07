use crate::heap::AllocError;
use crate::index_state::{HeapIndexState, PreparedIndexReclaim};
use crate::object::{ObjectRecord, OldRegionPlacement, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::spaces::{OldGenConfig, OldGenState, OldRegionCollectionStats, PreparedOldGenReclaim};
use crate::stats::{CollectionStats, HeapStats, PreparedHeapStats};

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

pub(crate) fn apply_prepared_reclaim(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    stats: &mut HeapStats,
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
    let old_reserved_bytes = old_gen.reserved_bytes();
    indexes.apply_prepared_reclaim(prepared_indexes);
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
