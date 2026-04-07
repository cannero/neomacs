use super::*;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::object::SpaceKind;

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<Leaf>()))
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
