use super::{Ephemeron, Weak, WeakCell};
use crate::descriptor::{Relocator, Trace, Tracer, fixed_type_desc};
use crate::object::{ObjectRecord, SpaceKind};

#[derive(Debug)]
struct WeakLeaf;

unsafe impl Trace for WeakLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[test]
fn weak_containers_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<WeakCell<WeakLeaf>>();
    assert_send_sync::<Ephemeron<WeakLeaf, WeakLeaf>>();
}

#[test]
fn weak_cell_round_trips_values() {
    let desc = Box::leak(Box::new(fixed_type_desc::<WeakLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, WeakLeaf).expect("allocate weak leaf");
    let target = unsafe { crate::root::Gc::<WeakLeaf>::from_erased(record.erased()) };
    let weak = WeakCell::new(Weak::new(target));

    assert_eq!(
        weak.target().map(|value| value.erase()),
        Some(record.erased())
    );
    weak.clear();
    assert_eq!(weak.target().map(|value| value.erase()), None);
}

#[test]
fn ephemeron_round_trips_key_and_value() {
    let desc = Box::leak(Box::new(fixed_type_desc::<WeakLeaf>()));
    let key_record =
        ObjectRecord::allocate(desc, SpaceKind::Old, WeakLeaf).expect("allocate ephemeron key");
    let value_record =
        ObjectRecord::allocate(desc, SpaceKind::Old, WeakLeaf).expect("allocate ephemeron value");
    let key = unsafe { crate::root::Gc::<WeakLeaf>::from_erased(key_record.erased()) };
    let value = unsafe { crate::root::Gc::<WeakLeaf>::from_erased(value_record.erased()) };
    let ephemeron = Ephemeron::new(Weak::new(key), Weak::new(value));

    assert_eq!(
        ephemeron.key().map(|object| object.erase()),
        Some(key_record.erased())
    );
    assert_eq!(
        ephemeron.value().map(|object| object.erase()),
        Some(value_record.erased())
    );

    ephemeron.clear();

    assert_eq!(ephemeron.key().map(|object| object.erase()), None);
    assert_eq!(ephemeron.value().map(|object| object.erase()), None);
}
