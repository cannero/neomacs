use core::alloc::Layout;
use core::cell::Cell;
use core::ptr::NonNull;
use std::alloc::{alloc, dealloc};

use crate::descriptor::{
    EphemeronVisitor, GcErased, Relocator, Trace, TypeDesc, TypeFlags, WeakProcessor,
};
use crate::heap::AllocError;

/// Coarse heap space identity.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpaceKind {
    Nursery,
    Old,
    Pinned,
    Large,
    Immortal,
}

/// High-level generation bucket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Generation {
    Young,
    Old,
    Immortal,
}

impl SpaceKind {
    pub(crate) fn initial_generation(self) -> Generation {
        match self {
            Self::Nursery => Generation::Young,
            Self::Old | Self::Pinned | Self::Large => Generation::Old,
            Self::Immortal => Generation::Immortal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OldRegionPlacement {
    pub(crate) region_index: usize,
    pub(crate) offset_bytes: usize,
    pub(crate) line_start: usize,
    pub(crate) line_count: usize,
}

/// Per-object header stored adjacent to the payload.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct ObjectHeader {
    desc: &'static TypeDesc,
    total_size: usize,
    payload_size: usize,
    payload_offset: usize,
    space: Cell<SpaceKind>,
    generation: Cell<Generation>,
    age: Cell<u8>,
    mark_bits: Cell<u8>,
    forwarding: Cell<Option<NonNull<ObjectHeader>>>,
    moved_out: Cell<bool>,
}

impl ObjectHeader {
    pub(crate) fn desc(&self) -> &'static TypeDesc {
        self.desc
    }

    pub(crate) fn total_size(&self) -> usize {
        self.total_size
    }

    pub(crate) fn space(&self) -> SpaceKind {
        self.space.get()
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.mark_bits.get() != 0
    }

    pub(crate) fn age(&self) -> u8 {
        self.age.get()
    }

    pub(crate) fn set_marked(&self, marked: bool) {
        self.mark_bits.set(u8::from(marked));
    }

    pub(crate) fn clear_mark(&self) {
        self.mark_bits.set(0);
    }

    pub(crate) fn forward_to(&self, new_header: NonNull<ObjectHeader>) {
        self.forwarding.set(Some(new_header));
        self.moved_out.set(true);
    }

    pub(crate) fn is_moved_out(&self) -> bool {
        self.moved_out.get()
    }

    pub(crate) unsafe fn payload_ptr(header: NonNull<Self>) -> NonNull<u8> {
        let header_ref = unsafe { header.as_ref() };
        let payload = unsafe { header.cast::<u8>().as_ptr().add(header_ref.payload_offset) };
        unsafe { NonNull::new_unchecked(payload) }
    }
}

/// Owned allocation record stored by the heap.
#[derive(Debug)]
pub(crate) struct ObjectRecord {
    base: NonNull<u8>,
    layout: Layout,
    header: NonNull<ObjectHeader>,
    old_region: Option<OldRegionPlacement>,
}

pub(crate) fn allocation_layout_for<T>() -> Result<(Layout, usize), AllocError> {
    let header_layout = Layout::new::<ObjectHeader>();
    let payload_layout = Layout::new::<T>();
    let (layout, payload_offset) = header_layout
        .extend(payload_layout)
        .map_err(|_| AllocError::LayoutOverflow)?;
    Ok((layout.pad_to_align(), payload_offset))
}

pub fn estimated_allocation_size<T>() -> Result<usize, AllocError> {
    allocation_layout_for::<T>().map(|(layout, _)| layout.size())
}

impl ObjectRecord {
    pub(crate) fn allocate<T: Trace + 'static>(
        desc: &'static TypeDesc,
        space: SpaceKind,
        value: T,
    ) -> Result<Self, AllocError> {
        let (layout, payload_offset) = allocation_layout_for::<T>()?;

        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw).ok_or(AllocError::OutOfMemory {
            requested_bytes: layout.size(),
        })?;
        let header = base.cast::<ObjectHeader>();

        unsafe {
            header.as_ptr().write(ObjectHeader {
                desc,
                total_size: layout.size(),
                payload_size: core::mem::size_of::<T>(),
                payload_offset,
                space: Cell::new(space),
                generation: Cell::new(space.initial_generation()),
                age: Cell::new(0),
                mark_bits: Cell::new(0),
                forwarding: Cell::new(None),
                moved_out: Cell::new(false),
            });
        }

        let payload = unsafe { base.as_ptr().add(payload_offset).cast::<T>() };
        unsafe {
            payload.write(value);
        }

        Ok(Self {
            base,
            layout,
            header,
            old_region: None,
        })
    }

    pub(crate) fn erased(&self) -> GcErased {
        unsafe { GcErased::from_header(self.header) }
    }

    pub(crate) fn header(&self) -> &ObjectHeader {
        unsafe { self.header.as_ref() }
    }

    pub(crate) fn header_ptr(&self) -> NonNull<ObjectHeader> {
        self.header
    }

    pub(crate) fn old_region_placement(&self) -> Option<OldRegionPlacement> {
        self.old_region
    }

    pub(crate) fn set_old_region_placement(&mut self, placement: OldRegionPlacement) {
        self.old_region = Some(placement);
    }

    pub(crate) fn total_size(&self) -> usize {
        self.header().total_size()
    }

    pub(crate) fn space(&self) -> SpaceKind {
        self.header().space()
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.header().is_marked()
    }

    pub(crate) fn set_marked(&self, marked: bool) {
        self.header().set_marked(marked);
    }

    pub(crate) fn clear_mark(&self) {
        self.header().clear_mark();
    }

    pub(crate) fn payload_ptr(&self) -> NonNull<u8> {
        unsafe { ObjectHeader::payload_ptr(self.header) }
    }

    pub(crate) fn trace_edges(&self, tracer: &mut dyn crate::descriptor::Tracer) {
        unsafe {
            (self.header().desc().trace)(self.payload_ptr().as_ptr(), tracer);
        }
    }

    pub(crate) fn process_weak_edges(&self, processor: &mut dyn WeakProcessor) {
        unsafe {
            (self.header().desc().process_weak)(self.payload_ptr().as_ptr(), processor);
        }
    }

    pub(crate) fn visit_ephemerons(&self, visitor: &mut dyn EphemeronVisitor) {
        unsafe {
            (self.header().desc().visit_ephemerons)(self.payload_ptr().as_ptr(), visitor);
        }
    }

    pub(crate) fn relocate_edges(&self, relocator: &mut dyn Relocator) {
        unsafe {
            (self.header().desc().relocate)(self.payload_ptr().as_ptr(), relocator);
        }
    }

    pub(crate) fn run_finalizer(&self) -> bool {
        let desc = self.header().desc();
        if self.header().is_moved_out() || !desc.flags.contains(TypeFlags::FINALIZABLE) {
            return false;
        }
        unsafe {
            (desc.finalize)(self.payload_ptr().as_ptr());
        }
        true
    }

    pub(crate) fn evacuate_to_space(&self, space: SpaceKind) -> Result<Self, AllocError> {
        let total_size = self.total_size();
        let layout = Layout::from_size_align(total_size, self.layout.align())
            .map_err(|_| AllocError::LayoutOverflow)?;
        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw).ok_or(AllocError::OutOfMemory {
            requested_bytes: layout.size(),
        })?;
        let header = base.cast::<ObjectHeader>();

        unsafe {
            header.as_ptr().write(ObjectHeader {
                desc: self.header().desc(),
                total_size,
                payload_size: self.header().payload_size,
                payload_offset: self.header().payload_offset,
                space: Cell::new(space),
                generation: Cell::new(space.initial_generation()),
                age: Cell::new(self.header().age.get().saturating_add(1)),
                mark_bits: Cell::new(0),
                forwarding: Cell::new(None),
                moved_out: Cell::new(false),
            });
        }

        let src = self.payload_ptr();
        let dst = unsafe { ObjectHeader::payload_ptr(header) };
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_ptr(), self.header().payload_size);
        }
        self.header().forward_to(header);

        Ok(Self {
            base,
            layout,
            header,
            old_region: None,
        })
    }
}

impl Drop for ObjectRecord {
    fn drop(&mut self) {
        unsafe {
            let header = self.header.as_ref();
            let payload = ObjectHeader::payload_ptr(self.header);
            if !header.is_moved_out() {
                (header.desc().drop_in_place)(payload.as_ptr());
            }
            dealloc(self.base.as_ptr(), self.layout);
        }
    }
}
