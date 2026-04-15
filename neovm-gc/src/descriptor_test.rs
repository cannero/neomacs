use crate::descriptor::{Trace, fixed_type_desc};

struct PlainLeaf;

unsafe impl Trace for PlainLeaf {
    fn trace(&self, _tracer: &mut dyn crate::descriptor::Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

struct DroppyLeaf;

impl Drop for DroppyLeaf {
    fn drop(&mut self) {}
}

unsafe impl Trace for DroppyLeaf {
    fn trace(&self, _tracer: &mut dyn crate::descriptor::Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

#[test]
fn fixed_type_desc_marks_whether_payload_needs_drop() {
    let plain = fixed_type_desc::<PlainLeaf>();
    let droppy = fixed_type_desc::<DroppyLeaf>();

    assert!(!plain.needs_drop);
    assert!(droppy.needs_drop);
}
