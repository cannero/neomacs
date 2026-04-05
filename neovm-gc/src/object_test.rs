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
