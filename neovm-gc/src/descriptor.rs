use core::ptr::NonNull;
use std::any::type_name;

use bitflags::bitflags;

use crate::object::ObjectHeader;

/// Erased managed-object pointer used by the collector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct GcErased(NonNull<ObjectHeader>);

// Safety: `GcErased` is a non-owning identity handle into the managed heap.
// Sharing or moving the handle between threads does not transfer ownership of
// the underlying object; heap lifetime and mutation are governed by the
// collector/heap protocols.
unsafe impl Send for GcErased {}
unsafe impl Sync for GcErased {}

/// Stable identity key for one managed-object header.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ObjectKey(usize);

unsafe impl Send for ObjectKey {}
unsafe impl Sync for ObjectKey {}

impl GcErased {
    /// Construct an erased GC pointer from a managed object header.
    ///
    /// # Safety
    ///
    /// `header` must point at an object header managed by the heap.
    pub(crate) unsafe fn from_header(header: NonNull<ObjectHeader>) -> Self {
        Self(header)
    }

    pub(crate) unsafe fn from_raw(raw: *mut ObjectHeader) -> Option<Self> {
        NonNull::new(raw).map(Self)
    }

    pub(crate) fn header(self) -> NonNull<ObjectHeader> {
        self.0
    }

    pub(crate) fn as_raw(self) -> *mut ObjectHeader {
        self.0.as_ptr()
    }

    pub(crate) fn object_key(self) -> ObjectKey {
        ObjectKey::from_header(self.0)
    }
}

impl ObjectKey {
    pub(crate) fn from_header(header: NonNull<ObjectHeader>) -> Self {
        Self(header.as_ptr() as usize)
    }
}

/// Policy describing whether and how objects may move across collections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MovePolicy {
    /// Object may move freely between spaces and during compaction.
    Movable,
    /// Object begins movable but may be promoted into pinned space.
    PromoteToPinned,
    /// Object must not move.
    Pinned,
    /// Object belongs in large-object space.
    LargeObject,
    /// Object is permanent and not collected.
    Immortal,
}

/// High-level object layout classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutKind {
    /// Fixed-size object layout.
    Fixed,
    /// Variable-sized trailing payload.
    Variable,
    /// Inline array payload.
    InlineArray,
    /// Object owns external backing storage.
    External,
}

bitflags! {
    /// Descriptor-level GC behavior flags.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub struct TypeFlags: u32 {
        /// Object participates in weak-reference processing.
        const WEAK = 1 << 0;
        /// Object requires finalization.
        const FINALIZABLE = 1 << 1;
        /// Object owns an externally visible identity.
        const EXTERNALLY_VISIBLE = 1 << 2;
        /// Object may appear in ephemeron processing.
        const EPHEMERON_KEY = 1 << 3;
    }
}

/// Collector callback used to trace one object.
pub type TraceFn = unsafe fn(*mut u8, &mut dyn Tracer);

/// Collector callback used to compute object size.
pub type SizeFn = unsafe fn(*mut u8) -> usize;

/// Collector callback used to drop object contents.
pub type DropFn = unsafe fn(*mut u8);

/// Collector callback used to run finalization logic before teardown.
pub type FinalizeFn = unsafe fn(*mut u8);

/// Collector callback used to rewrite strong edges after object evacuation.
pub type RelocateFn = unsafe fn(*mut u8, &mut dyn Relocator);

/// Collector callback used to process weak edges reachable from one object.
pub type ProcessWeakFn = unsafe fn(*mut u8, &mut dyn WeakProcessor);

/// Collector callback used to visit ephemeron pairs reachable from one object.
pub type VisitEphemeronFn = unsafe fn(*mut u8, &mut dyn EphemeronVisitor);

/// Runtime type descriptor for one managed object kind.
#[derive(Clone, Copy, Debug)]
pub struct TypeDesc {
    /// Human-readable type name for debugging and telemetry.
    pub name: &'static str,
    /// Trace callback for object edges.
    pub trace: TraceFn,
    /// Size callback for object storage.
    pub size: SizeFn,
    /// Drop callback for object teardown.
    pub drop_in_place: DropFn,
    /// Finalization callback for objects that require it.
    pub finalize: FinalizeFn,
    /// Strong-edge relocation callback.
    pub relocate: RelocateFn,
    /// Weak-edge processing callback.
    pub process_weak: ProcessWeakFn,
    /// Ephemeron visitation callback used during fixpoint tracing.
    pub visit_ephemerons: VisitEphemeronFn,
    /// Movement policy for the collector.
    pub move_policy: MovePolicy,
    /// High-level layout kind.
    pub layout_kind: LayoutKind,
    /// Additional behavior flags.
    pub flags: TypeFlags,
}

/// Trait implemented by managed object payloads.
///
/// # Safety
///
/// Implementors must report every GC edge reachable from `self`.
pub unsafe trait Trace {
    /// Report outgoing GC edges to `tracer`.
    fn trace(&self, tracer: &mut dyn Tracer);

    /// Rewrite strong edges after evacuation.
    fn relocate(&self, relocator: &mut dyn Relocator);

    /// Process weak edges held by this object.
    fn process_weak(&self, _processor: &mut dyn WeakProcessor) {}

    /// Run object-specific finalization before teardown.
    fn finalize(&self) {}

    /// Visit ephemeron pairs held by this object.
    fn visit_ephemerons(&self, _visitor: &mut dyn EphemeronVisitor) {}

    /// Human-readable type name for debugging and telemetry.
    fn type_name() -> &'static str
    where
        Self: Sized,
    {
        type_name::<Self>()
    }

    /// Preferred movement policy for this object kind.
    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Movable
    }

    /// High-level layout kind for this object kind.
    fn layout_kind() -> LayoutKind
    where
        Self: Sized,
    {
        LayoutKind::Fixed
    }

    /// Extra descriptor flags for this object kind.
    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::empty()
    }
}

/// Visitor surface used during tracing.
pub trait Tracer {
    /// Mark an erased managed object.
    fn mark_erased(&mut self, object: GcErased);
}

/// Collector surface used to rewrite strong references after evacuation.
pub trait Relocator {
    /// Return the post-evacuation location for `object`.
    fn relocate_erased(&mut self, object: GcErased) -> GcErased;
}

/// Collector surface used to decide whether weak edges should be retained.
pub trait WeakProcessor {
    /// Return the post-collection weak target, or `None` if it should clear.
    fn remap_or_drop(&mut self, object: GcErased) -> Option<GcErased>;
}

/// Collector surface used during ephemeron fixpoint tracing.
pub trait EphemeronVisitor {
    /// Visit one ephemeron key/value pair.
    fn visit_ephemeron(&mut self, key: GcErased, value: GcErased);
}

/// Trace one managed edge through an erased collector callback.
pub fn trace_edge<T: ?Sized>(tracer: &mut dyn Tracer, object: crate::root::Gc<T>) {
    tracer.mark_erased(object.erase());
}

unsafe fn trace_impl<T: Trace + 'static>(payload: *mut u8, tracer: &mut dyn Tracer) {
    let value = unsafe { &*payload.cast::<T>() };
    value.trace(tracer);
}

unsafe fn size_impl<T: Trace + 'static>(payload: *mut u8) -> usize {
    let value = unsafe { &*payload.cast::<T>() };
    core::mem::size_of_val(value)
}

unsafe fn drop_impl<T: Trace + 'static>(payload: *mut u8) {
    unsafe {
        payload.cast::<T>().drop_in_place();
    }
}

unsafe fn finalize_impl<T: Trace + 'static>(payload: *mut u8) {
    let value = unsafe { &*payload.cast::<T>() };
    value.finalize();
}

unsafe fn relocate_impl<T: Trace + 'static>(payload: *mut u8, relocator: &mut dyn Relocator) {
    let value = unsafe { &*payload.cast::<T>() };
    value.relocate(relocator);
}

unsafe fn process_weak_impl<T: Trace + 'static>(
    payload: *mut u8,
    processor: &mut dyn WeakProcessor,
) {
    let value = unsafe { &*payload.cast::<T>() };
    value.process_weak(processor);
}

unsafe fn visit_ephemerons_impl<T: Trace + 'static>(
    payload: *mut u8,
    visitor: &mut dyn EphemeronVisitor,
) {
    let value = unsafe { &*payload.cast::<T>() };
    value.visit_ephemerons(visitor);
}

pub(crate) fn fixed_type_desc<T: Trace + 'static>() -> TypeDesc {
    TypeDesc {
        name: T::type_name(),
        trace: trace_impl::<T>,
        size: size_impl::<T>,
        drop_in_place: drop_impl::<T>,
        finalize: finalize_impl::<T>,
        relocate: relocate_impl::<T>,
        process_weak: process_weak_impl::<T>,
        visit_ephemerons: visit_ephemerons_impl::<T>,
        move_policy: T::move_policy(),
        layout_kind: T::layout_kind(),
        flags: T::type_flags(),
    }
}
