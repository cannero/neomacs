use super::*;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::object::{ObjectRecord, SpaceKind};

#[derive(Debug)]
struct OldLeaf;

unsafe impl Trace for OldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn old_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<OldLeaf>()))
}

#[test]
fn old_gen_record_allocated_object_sets_placement_and_live_stats() {
    let mut object =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate object");
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig::default();

    let reserved_bytes = old_gen.record_allocated_object(&config, &mut object);

    let placement = object
        .old_region_placement()
        .expect("old object placement recorded");
    assert_eq!(placement.region_index, 0);
    assert_eq!(reserved_bytes, old_gen.reserved_bytes());
    assert_eq!(old_gen.regions.len(), 1);
    assert_eq!(old_gen.regions[0].live_bytes, object.total_size());
    assert_eq!(old_gen.regions[0].object_count, 1);
}
