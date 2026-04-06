use super::{RootSlot, RootStack};
use crate::descriptor::{GcErased, Relocator, Trace, Tracer, fixed_type_desc};
use crate::object::{ObjectRecord, SpaceKind};

#[derive(Debug)]
struct RootLeaf;

unsafe impl Trace for RootLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

struct NoopRelocator;

impl Relocator for NoopRelocator {
    fn relocate_erased(&mut self, object: GcErased) -> GcErased {
        object
    }
}

#[test]
fn root_stack_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<super::Gc<RootLeaf>>();
    assert_send_sync::<RootSlot>();
    assert_send_sync::<RootStack>();
}

#[test]
fn root_slot_round_trips_objects() {
    let desc = Box::leak(Box::new(fixed_type_desc::<RootLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, RootLeaf).expect("allocate root leaf");
    let slot = RootSlot::new(Some(record.erased()));

    assert_eq!(slot.get(), Some(record.erased()));
    slot.set(None);
    assert_eq!(slot.get(), None);
}

#[test]
fn root_stack_relocates_all_slots() {
    let desc = Box::leak(Box::new(fixed_type_desc::<RootLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, RootLeaf).expect("allocate root leaf");
    let mut root_stack = RootStack::default();
    let index = root_stack.push(record.erased());

    assert_eq!(root_stack.get(index), Some(record.erased()));

    let mut relocator = NoopRelocator;
    root_stack.relocate_all(&mut relocator);

    assert_eq!(root_stack.get(index), Some(record.erased()));
}
