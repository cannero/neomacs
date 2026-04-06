use super::{ObjectRecord, SpaceKind};
use crate::descriptor::{Relocator, Trace, Tracer, fixed_type_desc};

#[derive(Debug)]
struct MarkLeaf;

unsafe impl Trace for MarkLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[test]
fn mark_if_unmarked_is_idempotent() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, MarkLeaf).expect("allocate test record");

    assert!(record.mark_if_unmarked());
    assert!(record.is_marked());
    assert!(!record.mark_if_unmarked());
    record.clear_mark();
    assert!(!record.is_marked());
    assert!(record.mark_if_unmarked());
}

#[test]
fn object_header_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<super::ObjectHeader>();
}

#[test]
fn evacuating_object_marks_source_moved_out_and_ages_copy() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, MarkLeaf).expect("allocate test record");

    let evacuated = record
        .evacuate_to_space(SpaceKind::Old)
        .expect("evacuate test record");

    assert!(record.header().is_moved_out());
    assert_eq!(evacuated.header().space(), SpaceKind::Old);
    assert_eq!(evacuated.header().generation(), super::Generation::Old);
    assert_eq!(evacuated.header().age(), 1);
}
