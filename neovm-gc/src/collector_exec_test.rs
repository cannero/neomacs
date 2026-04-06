use super::*;
use crate::descriptor::{Trace, fixed_type_desc};
use crate::index_state::ObjectIndex;
use crate::root::RootStack;
use std::collections::HashMap;

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

fn object_index_for(objects: &[ObjectRecord]) -> ObjectIndex {
    objects
        .iter()
        .enumerate()
        .map(|(index, object)| (object.object_key(), index))
        .collect::<HashMap<_, _>>()
}

#[test]
fn trace_major_marks_seeded_source() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let source = object.erased();
    let objects = vec![object];
    let index = object_index_for(&objects);

    let (steps, rounds) = super::trace_major(&objects, &index, 1, 8, [source]);

    assert_eq!(steps, 1);
    assert_eq!(rounds, 1);
    assert!(objects[0].is_marked());
}

#[test]
fn trace_minor_marks_seeded_nursery_source() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery leaf");
    let source = object.erased();
    let objects = vec![object];
    let index = object_index_for(&objects);

    let (steps, rounds) = super::trace_minor(&objects, &index, &[], &[], 1, 8, [source]);

    assert_eq!(steps, 1);
    assert_eq!(rounds, 1);
    assert!(objects[0].is_marked());
}

#[test]
fn collect_global_sources_includes_roots_and_immortal_objects() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let rooted =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate rooted object");
    let immortal =
        ObjectRecord::allocate(desc, SpaceKind::Immortal, Leaf).expect("allocate immortal object");
    let nursery =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery object");
    let rooted_source = rooted.erased();
    let immortal_source = immortal.erased();
    let nursery_source = nursery.erased();
    let objects = vec![rooted, immortal, nursery];
    let mut roots = RootStack::default();
    roots.push(rooted_source);

    let sources = super::collect_global_sources(&roots, &objects);

    assert!(sources.contains(&rooted_source));
    assert!(sources.contains(&immortal_source));
    assert!(!sources.contains(&nursery_source));
}
