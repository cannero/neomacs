use super::*;
use crate::descriptor::{Trace, Tracer, TypeFlags, fixed_type_desc};
use crate::object::SpaceKind;
use crate::reclaim::PreparedReclaimSurvivor;

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<Leaf>()))
}

#[derive(Debug)]
struct FinalizableLeaf;

unsafe impl Trace for FinalizableLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::FINALIZABLE
    }
}

fn finalizable_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<FinalizableLeaf>()))
}

#[derive(Debug)]
struct WeakEphemeronLeaf;

unsafe impl Trace for WeakEphemeronLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::WEAK | TypeFlags::EPHEMERON_KEY
    }
}

fn weak_ephemeron_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<WeakEphemeronLeaf>()))
}

#[test]
fn remembered_set_record_owner_deduplicates() {
    // The owner-only fallback set dedupes by ObjectKey.
    // Recording the same owner twice (e.g. two different
    // nursery edges from the same pinned holder) should leave
    // exactly one entry in the set, not two.
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");

    let mut remembered = RememberedSetState::default();
    remembered.record_owner(owner.object_key());
    remembered.record_owner(owner.object_key());

    assert_eq!(remembered.owners, vec![owner.object_key()]);
}

#[test]
fn remembered_set_refresh_drops_owners_with_no_nursery_edges_when_index_lacks_owner() {
    // refresh_from_records should drop any owner whose
    // ObjectKey is no longer present in the rebuilt
    // object_index (i.e. the owner is dead). Use a synthetic
    // index that is missing the owner to simulate the dead
    // case without needing to set up a full owner record with
    // edge slots (the trace-edge predicate tests live in
    // public_api.rs and lib_test.rs because they need real
    // EdgeCell holders).
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");

    let objects: Vec<ObjectRecord> = Vec::new();
    let object_index = ObjectIndex::new();

    let mut remembered = RememberedSetState::default();
    remembered.record_owner(owner.object_key());
    assert_eq!(remembered.owners.len(), 1);

    remembered.refresh_from_records(&objects, &object_index);
    assert!(remembered.owners.is_empty());
}

#[test]
fn heap_index_state_record_allocated_object_updates_index_and_candidates() {
    let desc = leaf_desc();
    let object = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate object");
    let object_key = object.object_key();
    let mut indexes = HeapIndexState::default();

    indexes.record_allocated_object(object_key, 3, desc);

    assert_eq!(indexes.object_index.get(&object_key), Some(&3));
    assert!(indexes.finalizable_candidates.is_empty());
    assert!(indexes.weak_candidates.is_empty());
    assert!(indexes.ephemeron_candidates.is_empty());
}

#[test]
fn heap_index_state_prepare_reclaim_state_rebuilds_candidates_and_remembered_owners() {
    // The owner-only filter requires walking the owner's
    // edges via trace_edges() to confirm at least one nursery
    // reference. The bare `Leaf` test struct in this file does
    // not implement edges, so the prepare path filters out the
    // owner here. The substantive end-to-end remembered-set
    // contracts (with real EdgeCell holders) live in
    // public_api.rs and lib_test.rs. This test still verifies
    // the candidate rebuild path for finalizable, weak, and
    // ephemeron candidates and the finalize_indices output.
    let finalizable_desc = finalizable_leaf_desc();
    let weak_ephemeron_desc = weak_ephemeron_leaf_desc();
    let leaf_desc = leaf_desc();

    let dead_finalizable =
        ObjectRecord::allocate(finalizable_desc, SpaceKind::Pinned, FinalizableLeaf)
            .expect("allocate dead finalizable");
    let live_finalizable =
        ObjectRecord::allocate(finalizable_desc, SpaceKind::Pinned, FinalizableLeaf)
            .expect("allocate live finalizable");
    let live_weak_ephemeron =
        ObjectRecord::allocate(weak_ephemeron_desc, SpaceKind::Old, WeakEphemeronLeaf)
            .expect("allocate live weak ephemeron");
    let owner = ObjectRecord::allocate(leaf_desc, SpaceKind::Pinned, Leaf).expect("allocate owner");
    let target =
        ObjectRecord::allocate(leaf_desc, SpaceKind::Nursery, Leaf).expect("allocate target");

    assert!(live_finalizable.mark_if_unmarked());
    assert!(live_weak_ephemeron.mark_if_unmarked());
    assert!(owner.mark_if_unmarked());
    assert!(target.mark_if_unmarked());

    let objects = vec![
        dead_finalizable,
        live_finalizable,
        live_weak_ephemeron,
        owner,
        target,
    ];
    let mut indexes = HeapIndexState::default();
    for (index, object) in objects.iter().enumerate() {
        indexes.record_allocated_object(object.object_key(), index, object.header().desc());
    }
    indexes.record_remembered_owner(objects[3].erased());

    let survivors = vec![
        PreparedReclaimSurvivor { object_index: 1 },
        PreparedReclaimSurvivor { object_index: 2 },
        PreparedReclaimSurvivor { object_index: 3 },
        PreparedReclaimSurvivor { object_index: 4 },
    ];

    let prepared = indexes.prepare_reclaim_state(&objects, &survivors, CollectionKind::Major);

    assert_eq!(
        prepared.rebuilt_object_index.get(&objects[1].object_key()),
        Some(&0)
    );
    assert_eq!(
        prepared.rebuilt_object_index.get(&objects[2].object_key()),
        Some(&1)
    );
    assert_eq!(prepared.finalize_indices, vec![0]);
    assert_eq!(
        prepared.finalizable_candidates,
        vec![objects[1].object_key()]
    );
    assert_eq!(prepared.weak_candidates, vec![objects[2].object_key()]);
    assert_eq!(prepared.ephemeron_candidates, vec![objects[2].object_key()]);
    // The bare Leaf owner has no trace edges, so the live-
    // nursery-edge predicate filters it out at the prepare
    // step. The substantive contract is exercised in
    // public_api.rs's pinned-owner test.
    assert!(prepared.remembered_owners.is_empty());
}

#[test]
fn begin_post_sweep_rebuild_preserves_dead_finalizable_membership() {
    let finalizable_desc = finalizable_leaf_desc();
    let object = ObjectRecord::allocate(finalizable_desc, SpaceKind::Pinned, FinalizableLeaf)
        .expect("allocate finalizable object");
    let object_key = object.object_key();
    let mut indexes = HeapIndexState::default();
    indexes.record_allocated_object(object_key, 0, finalizable_desc);

    let rebuild = indexes.begin_post_sweep_rebuild(4);

    assert!(rebuild.should_enqueue_finalizer(&object));
    assert!(indexes.object_index.is_empty());
    assert!(indexes.finalizable_candidates.is_empty());
}

#[test]
fn heap_index_state_apply_storage_stats_reports_candidate_and_remembered_counts() {
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");
    let target =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery target");
    let finalizable =
        ObjectRecord::allocate(finalizable_leaf_desc(), SpaceKind::Pinned, FinalizableLeaf)
            .expect("allocate finalizable");
    let weak_ephemeron = ObjectRecord::allocate(
        weak_ephemeron_leaf_desc(),
        SpaceKind::Old,
        WeakEphemeronLeaf,
    )
    .expect("allocate weak ephemeron");
    let mut indexes = HeapIndexState::default();
    indexes.record_allocated_object(owner.object_key(), 0, desc);
    indexes.record_allocated_object(target.object_key(), 1, desc);
    indexes.record_allocated_object(finalizable.object_key(), 2, finalizable.header().desc());
    indexes.record_allocated_object(
        weak_ephemeron.object_key(),
        3,
        weak_ephemeron.header().desc(),
    );
    indexes.record_remembered_owner(owner.erased());

    let mut stats = crate::stats::HeapStats::default();
    indexes.apply_storage_stats(&mut stats);

    assert_eq!(stats.remembered_edges, 1);
    assert_eq!(stats.remembered_owners, 1);
    assert_eq!(stats.finalizable_candidates, 1);
    assert_eq!(stats.weak_candidates, 1);
    assert_eq!(stats.ephemeron_candidates, 1);
}

#[test]
fn heap_index_state_record_remembered_edge_if_needed_only_keeps_old_to_nursery() {
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");
    let nursery_target =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery target");
    let old_target =
        ObjectRecord::allocate(desc, SpaceKind::Old, Leaf).expect("allocate old target");
    let objects = vec![owner, nursery_target, old_target];
    let mut indexes = HeapIndexState::default();
    for (index, object) in objects.iter().enumerate() {
        indexes.record_allocated_object(object.object_key(), index, desc);
    }

    let old_gen = crate::spaces::OldGenState::default();
    indexes.record_remembered_edge_if_needed(
        &objects,
        &old_gen,
        objects[0].erased(),
        Some(objects[1].erased()),
    );
    indexes.record_remembered_edge_if_needed(
        &objects,
        &old_gen,
        objects[0].erased(),
        Some(objects[2].erased()),
    );

    // The barrier hot path records fallback owners through
    // `record_owner_shared`, which queues into the
    // `pending_inserts` mutex. Merge the pending entries into
    // the canonical set before asserting so the test sees the
    // same view the collector will during its next GC pass.
    indexes.remembered.merge_pending_owners();

    // Only the old-to-nursery write recorded the owner. The
    // old-to-old write was filtered out by the
    // record_remembered_edge_if_needed predicate.
    assert_eq!(indexes.remembered.owners, vec![objects[0].object_key()]);
}
