use std::collections::{HashMap, HashSet};

use crate::barrier::RememberedEdge;
use crate::descriptor::{ObjectKey, TypeFlags};
use crate::index_state::HeapIndexState;
use crate::object::{ObjectRecord, OldRegionPlacement, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::spaces::{OldGenConfig, OldGenState, OldRegion, OldRegionCollectionStats};

#[derive(Debug)]
pub(crate) struct PreparedReclaimSurvivor {
    /// Original index in `Heap::objects` before reclaim commit.
    pub(crate) object_index: usize,
    pub(crate) old_region_placement: Option<OldRegionPlacement>,
}

#[derive(Debug)]
pub(crate) struct PreparedReclaim {
    pub(crate) promoted_bytes: usize,
    pub(crate) rebuilt_old_regions: Vec<OldRegion>,
    pub(crate) rebuilt_object_index: HashMap<ObjectKey, usize>,
    pub(crate) old_reserved_bytes: usize,
    pub(crate) old_region_stats: OldRegionCollectionStats,
    /// Survivors in ascending original `object_index` order.
    ///
    /// `commit_prepared_reclaim_objects` drains this in lockstep with the original
    /// `objects` vector, so ordering is part of the prepared-state contract.
    pub(crate) survivors: Vec<PreparedReclaimSurvivor>,
    /// Dead finalizable object indices in ascending original `object_index`
    /// order. `commit_prepared_reclaim_objects` drains these into the
    /// pending-finalizer queue in lockstep with the original `objects` vector.
    pub(crate) finalize_indices: Vec<usize>,
    pub(crate) finalizable_candidates: Vec<ObjectKey>,
    pub(crate) weak_candidates: Vec<ObjectKey>,
    pub(crate) ephemeron_candidates: Vec<ObjectKey>,
    pub(crate) remembered_edges: Vec<RememberedEdge>,
    pub(crate) remembered_owners: Vec<ObjectKey>,
    pub(crate) nursery_live_bytes: usize,
    pub(crate) old_live_bytes: usize,
    pub(crate) pinned_live_bytes: usize,
    pub(crate) large_live_bytes: usize,
    pub(crate) immortal_live_bytes: usize,
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
    let mut rebuilt_object_index = HashMap::with_capacity(objects.len());
    let mut finalize_indices = Vec::new();
    let finalizable_candidate_set: HashSet<_> =
        indexes.finalizable_candidates.iter().copied().collect();
    let mut finalizable_candidates = Vec::new();
    let mut weak_candidates = Vec::new();
    let mut ephemeron_candidates = Vec::new();
    let mut nursery_live_bytes = 0usize;
    let mut old_live_bytes = 0usize;
    let mut pinned_live_bytes = 0usize;
    let mut large_live_bytes = 0usize;
    let mut immortal_live_bytes = 0usize;

    for (object_index, object) in objects.iter().enumerate() {
        let object_key = object.object_key();
        let desc = object.header().desc();
        if !keep_object_for_collection(kind, object) {
            if !object.header().is_moved_out() && finalizable_candidate_set.contains(&object_key) {
                finalize_indices.push(object_index);
            }
            continue;
        }

        let total_size = object.total_size();
        if finalizable_candidate_set.contains(&object_key) {
            finalizable_candidates.push(object_key);
        }
        if desc.flags.contains(TypeFlags::WEAK) {
            weak_candidates.push(object_key);
        }
        if desc.flags.contains(TypeFlags::EPHEMERON_KEY) {
            ephemeron_candidates.push(object_key);
        }

        let old_region_placement = match object.space() {
            SpaceKind::Old => {
                let mut placement = object
                    .old_region_placement()
                    .expect("live old object should retain old-region placement");
                if rebuild.selected_regions.contains(&placement.region_index) {
                    let compacted = OldGenState::reserve_rebuild_placement(
                        &mut rebuild.compacted_regions,
                        old_config,
                        total_size,
                    );
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
                } else if let Some(&new_index) =
                    rebuild.preserved_index_map.get(&placement.region_index)
                {
                    placement.region_index = new_index;
                    let region = &mut rebuild.rebuilt_regions[new_index];
                    region.live_bytes = region.live_bytes.saturating_add(total_size);
                    region.object_count = region.object_count.saturating_add(1);
                    for line in placement.line_start..placement.line_start + placement.line_count {
                        region.occupied_lines.insert(line);
                    }
                }
                Some(placement)
            }
            _ => None,
        };
        survivors.push(PreparedReclaimSurvivor {
            object_index,
            old_region_placement,
        });
        rebuilt_object_index.insert(object_key, survivors.len().saturating_sub(1));

        match object.space() {
            SpaceKind::Nursery => {
                nursery_live_bytes = nursery_live_bytes.saturating_add(total_size);
            }
            SpaceKind::Old => {
                old_live_bytes = old_live_bytes.saturating_add(total_size);
            }
            SpaceKind::Pinned => {
                pinned_live_bytes = pinned_live_bytes.saturating_add(total_size);
            }
            SpaceKind::Large => {
                large_live_bytes = large_live_bytes.saturating_add(total_size);
            }
            SpaceKind::Immortal => {
                immortal_live_bytes = immortal_live_bytes.saturating_add(total_size);
            }
        }
    }

    let (rebuilt_old_regions, old_region_stats) =
        OldGenState::finish_prepared_rebuild(rebuild, &mut survivors);
    let old_reserved_bytes = rebuilt_old_regions
        .iter()
        .map(|region| region.capacity_bytes)
        .sum();
    let (remembered_edges, remembered_owners) =
        indexes.remembered_edges_for_collection(objects, kind);
    PreparedReclaim {
        promoted_bytes: 0,
        rebuilt_old_regions,
        rebuilt_object_index,
        old_reserved_bytes,
        old_region_stats,
        survivors,
        finalize_indices,
        finalizable_candidates,
        weak_candidates,
        ephemeron_candidates,
        remembered_edges,
        remembered_owners,
        nursery_live_bytes,
        old_live_bytes,
        pinned_live_bytes,
        large_live_bytes,
        immortal_live_bytes,
    }
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
            .finalize_indices
            .windows(2)
            .all(|window| window[0] < window[1]),
        "prepared reclaim finalizer indices must stay sorted by original object index"
    );

    let mut queued_finalizers = 0u64;
    let mut survivor_iter = prepared_reclaim.survivors.iter().peekable();
    let mut finalize_iter = prepared_reclaim.finalize_indices.iter().copied().peekable();
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
