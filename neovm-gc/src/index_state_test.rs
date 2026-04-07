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
fn remembered_set_record_edge_deduplicates_owner_keys() {
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");
    let target_a =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery target");
    let target_b =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery target");

    let mut remembered = RememberedSetState::default();
    remembered.record_edge(owner.erased(), target_a.erased());
    remembered.record_edge(owner.erased(), target_b.erased());

    assert_eq!(remembered.edges.len(), 2);
    assert_eq!(remembered.owners, vec![owner.object_key()]);
}

#[test]
fn remembered_set_retain_for_post_sweep_objects_keeps_only_old_to_nursery_edges() {
    let desc = leaf_desc();
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner");
    let live_target =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery target");
    let old_target =
        ObjectRecord::allocate(desc, SpaceKind::Old, Leaf).expect("allocate old target");
    let dead_target =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate dead target");
    let objects = vec![owner, live_target, old_target, dead_target];
    let object_index = [
        (objects[0].object_key(), 0usize),
        (objects[1].object_key(), 1usize),
        (objects[2].object_key(), 2usize),
    ]
    .into_iter()
    .collect();

    let mut remembered = RememberedSetState::default();
    remembered.record_edge(objects[0].erased(), objects[1].erased());
    remembered.record_edge(objects[0].erased(), objects[2].erased());
    remembered.record_edge(objects[0].erased(), objects[3].erased());

    remembered.retain_for_post_sweep_objects(&objects, &object_index);

    assert_eq!(remembered.edges.len(), 1);
    assert_eq!(remembered.owners, vec![objects[0].object_key()]);
    assert_eq!(
        remembered.edges[0].target.erase().object_key(),
        objects[1].object_key()
    );
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
fn heap_index_state_prepare_reclaim_state_rebuilds_candidates_and_remembered_edges() {
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
    indexes.record_remembered_edge(objects[3].erased(), objects[4].erased());

    let survivors = vec![
        PreparedReclaimSurvivor {
            object_index: 1,
            old_region_placement: None,
        },
        PreparedReclaimSurvivor {
            object_index: 2,
            old_region_placement: objects[2].old_region_placement(),
        },
        PreparedReclaimSurvivor {
            object_index: 3,
            old_region_placement: None,
        },
        PreparedReclaimSurvivor {
            object_index: 4,
            old_region_placement: None,
        },
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
    assert_eq!(prepared.remembered_edges.len(), 1);
    assert_eq!(prepared.remembered_owners, vec![objects[3].object_key()]);
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
    indexes.record_remembered_edge(owner.erased(), target.erased());

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

    indexes.record_remembered_edge_if_needed(
        &objects,
        objects[0].erased(),
        Some(objects[1].erased()),
    );
    indexes.record_remembered_edge_if_needed(
        &objects,
        objects[0].erased(),
        Some(objects[2].erased()),
    );

    assert_eq!(indexes.remembered.edges.len(), 1);
    assert_eq!(indexes.remembered.owners, vec![objects[0].object_key()]);
    assert_eq!(
        indexes.remembered.edges[0].target.erase().object_key(),
        objects[1].object_key()
    );
}
