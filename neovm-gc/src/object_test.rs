use super::{ObjectRecord, PendingFinalizer, SpaceKind};
use crate::descriptor::{Relocator, Trace, Tracer, TypeFlags, fixed_type_desc};
use std::alloc::{alloc, dealloc};
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct MarkLeaf;

unsafe impl Trace for MarkLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

static FINALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct FinalizableLeaf;

unsafe impl Trace for FinalizableLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn finalize(&self) {
        FINALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn type_flags() -> TypeFlags {
        TypeFlags::FINALIZABLE
    }
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
fn object_record_stays_compact() {
    assert_eq!(size_of::<ObjectRecord>(), 24);
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

#[test]
fn old_space_arena_evacuation_stays_arena_owned() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, MarkLeaf).expect("allocate test record");
    let (layout, _) =
        super::allocation_layout_for::<MarkLeaf>().expect("derive test allocation layout");
    let base = unsafe { alloc(layout) };
    let base = std::ptr::NonNull::new(base).expect("allocate arena-style test slot");

    let evacuated = unsafe {
        record
            .evacuate_to_arena_slot(SpaceKind::Old, base)
            .expect("evacuate into old arena slot")
    };

    assert_eq!(evacuated.header().space(), SpaceKind::Old);
    assert!(evacuated.is_arena_owned());

    std::mem::forget(evacuated);
    unsafe {
        dealloc(base.as_ptr(), layout);
    }
}

#[test]
fn pending_finalizer_run_invokes_descriptor_finalize() {
    FINALIZE_COUNT.store(0, Ordering::SeqCst);
    let desc = Box::leak(Box::new(fixed_type_desc::<FinalizableLeaf>()));
    assert!(desc.flags.contains(TypeFlags::FINALIZABLE));
    let record = ObjectRecord::allocate(desc, SpaceKind::Old, FinalizableLeaf)
        .expect("allocate finalizable record");

    let pending = PendingFinalizer::new(record);
    assert!(pending.run());
    assert_eq!(FINALIZE_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn pending_finalizer_run_returns_false_for_non_finalizable_descriptor() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    assert!(!desc.flags.contains(TypeFlags::FINALIZABLE));
    let record = ObjectRecord::allocate(desc, SpaceKind::Old, MarkLeaf)
        .expect("allocate non-finalizable record");

    let pending = PendingFinalizer::new(record);
    assert!(!pending.run());
}

#[test]
fn pending_finalizer_block_placement_passes_through_wrapped_record() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    let mut record =
        ObjectRecord::allocate(desc, SpaceKind::Old, MarkLeaf).expect("allocate test record");
    let total_size = record.total_size();
    record.set_old_block_placement(super::OldBlockPlacement {
        block_index: 7,
        offset_bytes: 16,
        total_size,
    });

    let mut pending = PendingFinalizer::new(record);
    assert_eq!(
        pending
            .block_placement()
            .expect("placement set above")
            .block_index,
        7
    );

    pending.rebind_block(2);
    let placement = pending.block_placement().expect("placement still set");
    assert_eq!(placement.block_index, 2);
    assert_eq!(placement.offset_bytes, 16);
    assert_eq!(placement.total_size, total_size);
}

#[test]
fn pending_finalizer_block_placement_none_when_record_has_no_block() {
    let desc = Box::leak(Box::new(fixed_type_desc::<MarkLeaf>()));
    let record =
        ObjectRecord::allocate(desc, SpaceKind::Old, MarkLeaf).expect("allocate test record");
    let pending = PendingFinalizer::new(record);
    assert!(pending.block_placement().is_none());
}
