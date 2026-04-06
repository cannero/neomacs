use super::EdgeCell;
use crate::descriptor::{Relocator, Trace, Tracer, fixed_type_desc};
use crate::object::{ObjectRecord, SpaceKind};

#[derive(Debug)]
struct EdgeLeaf;

unsafe impl Trace for EdgeLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[test]
fn edge_cell_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<EdgeCell<EdgeLeaf>>();
}

#[test]
fn edge_cell_round_trips_values() {
    let desc = Box::leak(Box::new(fixed_type_desc::<EdgeLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, EdgeLeaf).expect("allocate edge leaf");
    let edge = EdgeCell::new(Some(unsafe {
        crate::root::Gc::<EdgeLeaf>::from_erased(record.erased())
    }));

    assert_eq!(edge.get().map(|value| value.erase()), Some(record.erased()));
    assert_eq!(
        edge.replace(None).map(|value| value.erase()),
        Some(record.erased())
    );
    assert_eq!(edge.get().map(|value| value.erase()), None);
}
