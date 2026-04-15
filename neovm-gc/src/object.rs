use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::alloc::{alloc, dealloc};

use crate::descriptor::{
    EphemeronVisitor, GcErased, ObjectKey, Relocator, Trace, TypeDesc, TypeFlags, WeakProcessor,
};
use crate::heap::AllocError;

/// Coarse heap space identity.
#[allow(dead_code)]
#[repr(u8)]
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
#[repr(u8)]
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

    fn from_u8(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Nursery as u8 => Self::Nursery,
            raw if raw == Self::Old as u8 => Self::Old,
            raw if raw == Self::Pinned as u8 => Self::Pinned,
            raw if raw == Self::Large as u8 => Self::Large,
            raw if raw == Self::Immortal as u8 => Self::Immortal,
            _ => panic!("invalid space kind byte: {raw}"),
        }
    }
}

impl Generation {
    #[allow(dead_code)]
    fn from_u8(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Young as u8 => Self::Young,
            raw if raw == Self::Old as u8 => Self::Old,
            raw if raw == Self::Immortal as u8 => Self::Immortal,
            _ => panic!("invalid generation byte: {raw}"),
        }
    }
}

/// Physical placement of an object inside an `OldBlock` from `OldGenState`.
///
/// Records which concrete block buffer the object's bytes live in so the
/// sweep path can re-mark the lines the object occupies and so empty blocks
/// can be reclaimed after collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OldBlockPlacement {
    pub(crate) block_index: usize,
    pub(crate) offset_bytes: usize,
    pub(crate) total_size: usize,
}

/// Per-object header stored adjacent to the payload.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct ObjectHeader {
    desc: &'static TypeDesc,
    total_size: usize,
    payload_size: usize,
    payload_offset: usize,
    space: AtomicU8,
    generation: AtomicU8,
    age: AtomicU8,
    mark_bits: AtomicU8,
    forwarding: AtomicPtr<ObjectHeader>,
    moved_out: AtomicBool,
}

const OBJECT_HEADER_TEMPLATE_SIZE: usize = core::mem::size_of::<ObjectHeader>();

#[derive(Clone, Copy, Debug)]
pub(crate) struct ObjectHeaderTemplate {
    bytes: [u8; OBJECT_HEADER_TEMPLATE_SIZE],
}

impl ObjectHeaderTemplate {
    pub(crate) fn for_allocation<T: Trace + 'static>(
        desc: &'static TypeDesc,
        space: SpaceKind,
        total_size: usize,
        payload_offset: usize,
        age: u8,
    ) -> Self {
        let header = MaybeUninit::new(ObjectHeader {
            desc,
            total_size,
            payload_size: core::mem::size_of::<T>(),
            payload_offset,
            space: AtomicU8::new(space as u8),
            generation: AtomicU8::new(space.initial_generation() as u8),
            age: AtomicU8::new(age),
            mark_bits: AtomicU8::new(0),
            forwarding: AtomicPtr::new(core::ptr::null_mut()),
            moved_out: AtomicBool::new(false),
        });
        let mut bytes = [0_u8; OBJECT_HEADER_TEMPLATE_SIZE];
        unsafe {
            core::ptr::copy_nonoverlapping(
                header.as_ptr().cast::<u8>(),
                bytes.as_mut_ptr(),
                OBJECT_HEADER_TEMPLATE_SIZE,
            );
        }
        Self { bytes }
    }

    #[inline(always)]
    unsafe fn write_to(self, header: NonNull<ObjectHeader>) {
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.bytes.as_ptr(),
                header.as_ptr().cast::<u8>(),
                OBJECT_HEADER_TEMPLATE_SIZE,
            );
        }
    }
}

impl ObjectHeader {
    pub(crate) fn desc(&self) -> &'static TypeDesc {
        self.desc
    }

    pub(crate) fn total_size(&self) -> usize {
        self.total_size
    }

    pub(crate) fn space(&self) -> SpaceKind {
        SpaceKind::from_u8(self.space.load(Ordering::Acquire))
    }

    #[allow(dead_code)]
    pub(crate) fn generation(&self) -> Generation {
        Generation::from_u8(self.generation.load(Ordering::Acquire))
    }

    pub(crate) fn is_marked(&self) -> bool {
        self.mark_bits.load(Ordering::Acquire) != 0
    }

    pub(crate) fn age(&self) -> u8 {
        self.age.load(Ordering::Acquire)
    }

    pub(crate) fn set_marked(&self, marked: bool) {
        self.mark_bits.store(u8::from(marked), Ordering::Release);
    }

    pub(crate) fn clear_mark(&self) {
        self.mark_bits.store(0, Ordering::Release);
    }

    pub(crate) fn mark_if_unmarked(&self) -> bool {
        self.mark_bits
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn forward_to(&self, new_header: NonNull<ObjectHeader>) {
        self.forwarding
            .store(new_header.as_ptr(), Ordering::Release);
        self.moved_out.store(true, Ordering::Release);
    }

    /// Atomically install a forwarding pointer if none has been
    /// installed yet (parallel-evacuation safe).
    ///
    /// Returns `Ok(())` if this caller installed `new_header` —
    /// the caller now owns the right to publish a new `ObjectRecord`
    /// for `new_header`. Returns `Err(existing)` if another worker
    /// already installed a forwarding pointer; the caller must
    /// discard its candidate copy and use the winning forwarding
    /// pointer instead.
    ///
    /// Note: callers that win the race must additionally set
    /// `moved_out` so the rest of the collector observes the source
    /// as moved. The `moved_out` store happens via a follow-up
    /// `Release` write inside `try_evacuate_to_arena_slot`.
    pub(crate) fn try_install_forwarding(
        &self,
        new_header: NonNull<ObjectHeader>,
    ) -> Result<(), NonNull<ObjectHeader>> {
        match self.forwarding.compare_exchange(
            core::ptr::null_mut(),
            new_header.as_ptr(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // Publish the moved-out flag after the forwarding
                // pointer is visible. Use Release so any worker that
                // observes `is_moved_out() == true` is guaranteed to
                // see the forwarding pointer it was paired with.
                self.moved_out.store(true, Ordering::Release);
                Ok(())
            }
            Err(existing) => {
                // SAFETY: a non-null forwarding pointer is only ever
                // installed via this CAS or `forward_to`, both of
                // which take a `NonNull<ObjectHeader>`.
                Err(unsafe { NonNull::new_unchecked(existing) })
            }
        }
    }

    pub(crate) fn is_moved_out(&self) -> bool {
        self.moved_out.load(Ordering::Acquire)
    }

    pub(crate) unsafe fn payload_ptr(header: NonNull<Self>) -> NonNull<u8> {
        let header_ref = unsafe { header.as_ref() };
        let payload = unsafe { header.cast::<u8>().as_ptr().add(header_ref.payload_offset) };
        unsafe { NonNull::new_unchecked(payload) }
    }
}

/// Backing-store identity for the memory underneath one `ObjectRecord`.
///
/// - `Owned` records were allocated via `std::alloc::alloc` and must
///   release their memory via `dealloc` on Drop.
/// - `Arena` records were bump-allocated from a `NurseryArena` and must
///   NOT touch the system allocator on Drop — the arena owns the
///   backing buffer and reclaims it in bulk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ObjectMemoryKind {
    Owned,
    Arena,
}

const NO_OLD_BLOCK_INDEX: u32 = u32::MAX;

/// Owned allocation record stored by the heap.
#[derive(Debug)]
pub(crate) struct ObjectRecord {
    header: NonNull<ObjectHeader>,
    old_block_index: u32,
    old_block_offset_bytes: u32,
    old_block_total_size: u32,
    layout_align_shift: u8,
    memory_kind: ObjectMemoryKind,
}

// Safety: `ObjectRecord` is the heap-owned metadata for one allocation. The raw
// pointers are stable allocation identities; sharing the record does not grant
// independent ownership of the allocation, and mutation/reclamation still flows
// through heap collection protocols.
unsafe impl Send for ObjectRecord {}
unsafe impl Sync for ObjectRecord {}

pub(crate) fn allocation_layout_for<T>() -> Result<(Layout, usize), AllocError> {
    let header_layout = Layout::new::<ObjectHeader>();
    let payload_layout = Layout::new::<T>();
    let (layout, payload_offset) = header_layout
        .extend(payload_layout)
        .map_err(|_| AllocError::LayoutOverflow)?;
    Ok((layout.pad_to_align(), payload_offset))
}

/// Estimated allocation footprint of a `T`-payload `ObjectRecord`,
/// including the header and any line/alignment padding the
/// allocator pipeline will pad up to. Returns `Err(LayoutOverflow)`
/// if the layout cannot be computed.
///
/// Useful for callers (e.g. the pacer's allocation accounting)
/// that need to know how many bytes a `Heap::alloc::<T>()` will
/// charge against the heap before actually allocating.
pub fn estimated_allocation_size<T>() -> Result<usize, AllocError> {
    allocation_layout_for::<T>().map(|(layout, _)| layout.size())
}

impl ObjectRecord {
    #[inline]
    fn layout_align_to_shift(layout_align: usize) -> u8 {
        debug_assert!(layout_align.is_power_of_two());
        u8::try_from(layout_align.trailing_zeros())
            .expect("layout alignment shift should fit in u8")
    }

    #[inline]
    fn layout_align_from_shift(layout_align_shift: u8) -> usize {
        1usize << usize::from(layout_align_shift)
    }

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

        unsafe {
            Self::write_header_and_payload::<T>(base, layout, payload_offset, desc, space, 0, value)
        };
        let header = base.cast::<ObjectHeader>();

        Ok(Self {
            header,
            old_block_index: NO_OLD_BLOCK_INDEX,
            old_block_offset_bytes: 0,
            old_block_total_size: 0,
            layout_align_shift: Self::layout_align_to_shift(layout.align()),
            memory_kind: ObjectMemoryKind::Owned,
        })
    }

    pub(crate) fn allocate_owned_raw<T: Trace + 'static>(
        desc: &'static TypeDesc,
        space: SpaceKind,
        value: T,
    ) -> Result<(NonNull<ObjectHeader>, u8), AllocError> {
        let (layout, payload_offset) = allocation_layout_for::<T>()?;
        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw).ok_or(AllocError::OutOfMemory {
            requested_bytes: layout.size(),
        })?;
        unsafe {
            Self::write_header_and_payload::<T>(base, layout, payload_offset, desc, space, 0, value)
        };
        Ok((
            base.cast::<ObjectHeader>(),
            Self::layout_align_to_shift(layout.align()),
        ))
    }

    pub(crate) fn allocate_owned_raw_with_template<T: Trace + 'static>(
        header_template: ObjectHeaderTemplate,
        layout: Layout,
        payload_offset: usize,
        value: T,
    ) -> Result<(NonNull<ObjectHeader>, u8), AllocError> {
        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw).ok_or(AllocError::OutOfMemory {
            requested_bytes: layout.size(),
        })?;
        unsafe {
            Self::write_header_from_template_and_payload(
                base,
                payload_offset,
                header_template,
                value,
            )
        };
        Ok((
            base.cast::<ObjectHeader>(),
            Self::layout_align_to_shift(layout.align()),
        ))
    }

    /// Construct an `ObjectRecord` that points at a region already
    /// bump-allocated inside a `NurseryArena`. The caller has already
    /// reserved a `layout`-sized, `layout`-aligned region at `base`
    /// inside the arena; this constructor writes the `ObjectHeader`
    /// and the payload in place and records the memory kind as
    /// `Arena` so Drop skips the system allocator.
    ///
    /// # Safety
    ///
    /// - `base` must point to exactly `layout.size()` bytes of
    ///   uninitialized storage inside a nursery arena buffer.
    /// - The storage must not be reused or freed for the lifetime of
    ///   the returned `ObjectRecord`.
    /// - `layout` and `payload_offset` must match the results of
    ///   `allocation_layout_for::<T>()`.
    pub(crate) unsafe fn allocate_in_arena<T: Trace + 'static>(
        desc: &'static TypeDesc,
        space: SpaceKind,
        base: NonNull<u8>,
        layout: Layout,
        payload_offset: usize,
        value: T,
    ) -> Self {
        unsafe {
            Self::write_header_and_payload::<T>(base, layout, payload_offset, desc, space, 0, value)
        };
        let header = base.cast::<ObjectHeader>();
        Self {
            header,
            old_block_index: NO_OLD_BLOCK_INDEX,
            old_block_offset_bytes: 0,
            old_block_total_size: 0,
            layout_align_shift: Self::layout_align_to_shift(layout.align()),
            memory_kind: ObjectMemoryKind::Arena,
        }
    }

    pub(crate) unsafe fn allocate_in_arena_raw<T: Trace + 'static>(
        desc: &'static TypeDesc,
        space: SpaceKind,
        base: NonNull<u8>,
        layout: Layout,
        payload_offset: usize,
        value: T,
    ) -> (NonNull<ObjectHeader>, u8) {
        unsafe {
            Self::write_header_and_payload::<T>(base, layout, payload_offset, desc, space, 0, value)
        };
        (
            base.cast::<ObjectHeader>(),
            Self::layout_align_to_shift(layout.align()),
        )
    }

    pub(crate) unsafe fn allocate_in_arena_raw_with_template<T: Trace + 'static>(
        header_template: ObjectHeaderTemplate,
        base: NonNull<u8>,
        layout: Layout,
        payload_offset: usize,
        value: T,
    ) -> (NonNull<ObjectHeader>, u8) {
        unsafe {
            Self::write_header_from_template_and_payload(
                base,
                payload_offset,
                header_template,
                value,
            )
        };
        (
            base.cast::<ObjectHeader>(),
            Self::layout_align_to_shift(layout.align()),
        )
    }

    /// Low-level helper: write the ObjectHeader and the payload at `base`.
    ///
    /// # Safety
    ///
    /// Same constraints as `allocate_in_arena`: `base` must point to
    /// `layout.size()` bytes of suitably aligned uninitialized storage
    /// that outlives the resulting `ObjectRecord`.
    unsafe fn write_header_and_payload<T: Trace + 'static>(
        base: NonNull<u8>,
        layout: Layout,
        payload_offset: usize,
        desc: &'static TypeDesc,
        space: SpaceKind,
        age: u8,
        value: T,
    ) {
        let header = base.cast::<ObjectHeader>();
        unsafe {
            header.as_ptr().write(ObjectHeader {
                desc,
                total_size: layout.size(),
                payload_size: core::mem::size_of::<T>(),
                payload_offset,
                space: AtomicU8::new(space as u8),
                generation: AtomicU8::new(space.initial_generation() as u8),
                age: AtomicU8::new(age),
                mark_bits: AtomicU8::new(0),
                forwarding: AtomicPtr::new(core::ptr::null_mut()),
                moved_out: AtomicBool::new(false),
            });
            let payload = base.as_ptr().add(payload_offset).cast::<T>();
            payload.write(value);
        }
    }

    unsafe fn write_header_from_template_and_payload<T: Trace + 'static>(
        base: NonNull<u8>,
        payload_offset: usize,
        header_template: ObjectHeaderTemplate,
        value: T,
    ) {
        let header = base.cast::<ObjectHeader>();
        unsafe {
            header_template.write_to(header);
            let payload = base.as_ptr().add(payload_offset).cast::<T>();
            payload.write(value);
        }
    }

    pub(crate) unsafe fn write_published_record(
        slot: *mut MaybeUninit<Self>,
        header: NonNull<ObjectHeader>,
        old_block_placement: Option<OldBlockPlacement>,
        layout_align_shift: u8,
        memory_kind: ObjectMemoryKind,
    ) {
        let (old_block_index, old_block_offset_bytes, old_block_total_size) =
            match old_block_placement {
                Some(placement) => (
                    u32::try_from(placement.block_index)
                        .expect("old block index should fit compact ObjectRecord storage"),
                    u32::try_from(placement.offset_bytes)
                        .expect("old block offset should fit compact ObjectRecord storage"),
                    u32::try_from(placement.total_size)
                        .expect("old block size should fit compact ObjectRecord storage"),
                ),
                None => (NO_OLD_BLOCK_INDEX, 0, 0),
            };
        unsafe {
            (*slot).write(Self {
                header,
                old_block_index,
                old_block_offset_bytes,
                old_block_total_size,
                layout_align_shift,
                memory_kind,
            })
        };
    }

    pub(crate) fn erased(&self) -> GcErased {
        unsafe { GcErased::from_header(self.header) }
    }

    pub(crate) fn header(&self) -> &ObjectHeader {
        unsafe { self.header.as_ref() }
    }

    pub(crate) fn object_key(&self) -> ObjectKey {
        ObjectKey::from_header(self.header)
    }

    pub(crate) fn old_block_placement(&self) -> Option<OldBlockPlacement> {
        (self.old_block_index != NO_OLD_BLOCK_INDEX).then_some(OldBlockPlacement {
            block_index: self.old_block_index as usize,
            offset_bytes: self.old_block_offset_bytes as usize,
            total_size: self.old_block_total_size as usize,
        })
    }

    pub(crate) fn set_old_block_placement(&mut self, placement: OldBlockPlacement) {
        self.old_block_index = u32::try_from(placement.block_index)
            .expect("old block index should fit compact ObjectRecord storage");
        self.old_block_offset_bytes = u32::try_from(placement.offset_bytes)
            .expect("old block offset should fit compact ObjectRecord storage");
        self.old_block_total_size = u32::try_from(placement.total_size)
            .expect("old block size should fit compact ObjectRecord storage");
    }

    #[allow(dead_code)]
    pub(crate) fn clear_old_block_placement(&mut self) {
        self.old_block_index = NO_OLD_BLOCK_INDEX;
        self.old_block_offset_bytes = 0;
        self.old_block_total_size = 0;
    }

    pub(crate) fn total_size(&self) -> usize {
        self.header().total_size()
    }

    /// Alignment of the backing storage, needed when a copy target
    /// (e.g. the nursery to-space arena) needs to reserve a region
    /// with matching layout constraints.
    pub(crate) fn layout_align(&self) -> usize {
        Self::layout_align_from_shift(self.layout_align_shift)
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

    pub(crate) fn mark_if_unmarked(&self) -> bool {
        self.header().mark_if_unmarked()
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

    /// Returns true if this record's backing memory is owned by a
    /// nursery arena (not the system allocator).
    #[allow(dead_code)]
    pub(crate) fn is_arena_owned(&self) -> bool {
        matches!(self.memory_kind, ObjectMemoryKind::Arena)
    }

    #[inline]
    pub(crate) fn needs_record_drop(&self) -> bool {
        self.header().desc().needs_drop || matches!(self.memory_kind, ObjectMemoryKind::Owned)
    }

    #[inline]
    pub(crate) unsafe fn published_record_needs_drop(
        header: NonNull<ObjectHeader>,
        memory_kind: ObjectMemoryKind,
    ) -> bool {
        unsafe { header.as_ref() }.desc().needs_drop
            || matches!(memory_kind, ObjectMemoryKind::Owned)
    }

    /// Evacuate this record into a newly system-allocated backing
    /// store for `space`. The new record is always `ObjectMemoryKind::Owned`
    /// (promotion into old / pinned / large always goes through the
    /// system allocator; nursery-to-nursery survivor copies instead
    /// use `evacuate_to_arena_slot`).
    pub(crate) fn evacuate_to_space(&self, space: SpaceKind) -> Result<Self, AllocError> {
        let total_size = self.total_size();
        let layout = Layout::from_size_align(total_size, self.layout_align())
            .map_err(|_| AllocError::LayoutOverflow)?;
        let raw = unsafe { alloc(layout) };
        let base = NonNull::new(raw).ok_or(AllocError::OutOfMemory {
            requested_bytes: layout.size(),
        })?;
        unsafe { self.populate_evacuated_header(base, layout, space) };
        let header = base.cast::<ObjectHeader>();
        self.header().forward_to(header);
        Ok(Self {
            header,
            old_block_index: NO_OLD_BLOCK_INDEX,
            old_block_offset_bytes: 0,
            old_block_total_size: 0,
            layout_align_shift: Self::layout_align_to_shift(layout.align()),
            memory_kind: ObjectMemoryKind::Owned,
        })
    }

    /// Copy this record's header + payload into a caller-provided
    /// nursery arena slot. Returns a new `ObjectRecord` pointing at the
    /// arena slot with `ObjectMemoryKind::Arena`. The caller has
    /// already bump-allocated `layout.size()` aligned bytes at `base`.
    ///
    /// # Safety
    ///
    /// - `base` must point to at least `self.total_size()` bytes of
    ///   uninitialized storage inside a nursery arena buffer with
    ///   alignment matching `self.layout_align`.
    /// - The backing storage must outlive the returned record.
    pub(crate) unsafe fn evacuate_to_arena_slot(
        &self,
        space: SpaceKind,
        base: NonNull<u8>,
    ) -> Result<Self, AllocError> {
        let total_size = self.total_size();
        let layout = Layout::from_size_align(total_size, self.layout_align())
            .map_err(|_| AllocError::LayoutOverflow)?;
        unsafe { self.populate_evacuated_header(base, layout, space) };
        let header = base.cast::<ObjectHeader>();
        self.header().forward_to(header);
        Ok(Self {
            header,
            old_block_index: NO_OLD_BLOCK_INDEX,
            old_block_offset_bytes: 0,
            old_block_total_size: 0,
            layout_align_shift: Self::layout_align_to_shift(layout.align()),
            memory_kind: ObjectMemoryKind::Arena,
        })
    }

    /// Parallel-evacuation variant of `evacuate_to_arena_slot`.
    ///
    /// Writes the new header + payload at `base`, then atomically
    /// installs a forwarding pointer via CAS on
    /// `ObjectHeader::forwarding`. The first writer wins.
    ///
    /// Returns:
    /// - `Ok(Some(record))` if this caller installed the forwarding
    ///   pointer. The returned record now owns the arena slot and
    ///   should be added to `Heap::objects`.
    /// - `Ok(None)` if another worker already evacuated this object.
    ///   The bytes written at `base` are now dead — they remain in
    ///   the worker's slab but are not referenced by any record. The
    ///   bytes will be reclaimed in bulk when the slab is reset on
    ///   the next minor GC cycle. Callers that need the winning
    ///   forwarding pointer should consult
    ///   `self.header().forwarding`.
    /// - `Err(_)` on layout overflow.
    ///
    /// # Safety
    ///
    /// Same constraints as `evacuate_to_arena_slot`.
    pub(crate) unsafe fn try_evacuate_to_arena_slot(
        &self,
        space: SpaceKind,
        base: NonNull<u8>,
    ) -> Result<Option<Self>, AllocError> {
        let total_size = self.total_size();
        let layout = Layout::from_size_align(total_size, self.layout_align())
            .map_err(|_| AllocError::LayoutOverflow)?;
        // Speculatively write the candidate header + payload. If we
        // lose the CAS race below, the bytes become dead arena
        // memory.
        unsafe { self.populate_evacuated_header(base, layout, space) };
        let header = base.cast::<ObjectHeader>();
        match self.header().try_install_forwarding(header) {
            Ok(()) => Ok(Some(Self {
                header,
                old_block_index: NO_OLD_BLOCK_INDEX,
                old_block_offset_bytes: 0,
                old_block_total_size: 0,
                layout_align_shift: Self::layout_align_to_shift(layout.align()),
                memory_kind: ObjectMemoryKind::Arena,
            })),
            Err(_winner) => {
                // The header we wrote at `base` will never be
                // referenced again. We must NOT drop it as a normal
                // ObjectRecord (that would call drop_in_place on a
                // payload that's about to be aliased by the winner's
                // existing copy or recycled). Instead leave the bytes
                // in place; the slab is reclaimed in bulk on the next
                // reset.
                Ok(None)
            }
        }
    }

    /// Write the evacuated ObjectHeader and copy the payload bytes from
    /// `self` to `base`.
    ///
    /// # Safety
    ///
    /// `base` must point to `layout.size()` bytes of uninitialized
    /// storage matching `layout.align()`.
    unsafe fn populate_evacuated_header(
        &self,
        base: NonNull<u8>,
        layout: Layout,
        space: SpaceKind,
    ) {
        let total_size = self.total_size();
        let header = base.cast::<ObjectHeader>();
        unsafe {
            header.as_ptr().write(ObjectHeader {
                desc: self.header().desc(),
                total_size,
                payload_size: self.header().payload_size,
                payload_offset: self.header().payload_offset,
                space: AtomicU8::new(space as u8),
                generation: AtomicU8::new(space.initial_generation() as u8),
                age: AtomicU8::new(self.header().age().saturating_add(1)),
                mark_bits: AtomicU8::new(0),
                forwarding: AtomicPtr::new(core::ptr::null_mut()),
                moved_out: AtomicBool::new(false),
            });
        }
        let _ = layout;
        let src = self.payload_ptr();
        let dst = unsafe { ObjectHeader::payload_ptr(header) };
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_ptr(), self.header().payload_size);
        }
    }
}

impl Drop for ObjectRecord {
    fn drop(&mut self) {
        unsafe {
            let header = self.header.as_ref();
            let payload = ObjectHeader::payload_ptr(self.header);
            if header.desc().needs_drop && !header.is_moved_out() {
                (header.desc().drop_in_place)(payload.as_ptr());
            }
            // Arena-backed records must NOT touch the system allocator:
            // the arena owns the backing buffer and reclaims it in bulk
            // when it resets.
            if matches!(self.memory_kind, ObjectMemoryKind::Owned) {
                let layout = Layout::from_size_align(header.total_size(), self.layout_align())
                    .expect("object record layout should remain valid");
                dealloc(self.header.cast::<u8>().as_ptr(), layout);
            }
        }
    }
}

/// Lightweight VM-facing handoff for one queued finalizer.
///
/// Wraps an `ObjectRecord` whose owning slot in the heap's main
/// `objects` vector has been removed but whose finalizer has not
/// yet run. The handoff exposes a focused API for the runtime
/// state and reclaim paths to interact with: invoke the
/// finalizer, query the block placement that pins the source
/// block, or rebind the block index after physical reclamation.
///
/// Hiding `ObjectRecord` behind this newtype gives the
/// finalization queue a small, abstract surface so callers don't
/// reach into private record internals (header / base pointer /
/// memory kind) just to drive the drain loop.
///
/// Like `ObjectRecord`, dropping a `PendingFinalizer` runs the
/// payload's `drop_in_place` and frees the backing storage if
/// the record was system-allocated.
#[derive(Debug)]
pub(crate) struct PendingFinalizer {
    record: ObjectRecord,
}

impl PendingFinalizer {
    /// Wrap an `ObjectRecord` whose finalizer is pending. The
    /// caller should have already verified that the record's
    /// descriptor advertises `TypeFlags::FINALIZABLE`.
    pub(crate) fn new(record: ObjectRecord) -> Self {
        Self { record }
    }

    /// Invoke the wrapped record's finalizer. Returns `true` if
    /// the finalizer ran, `false` if the record was already
    /// moved out or its descriptor does not declare
    /// `FINALIZABLE`. Mirrors `ObjectRecord::run_finalizer`.
    pub(crate) fn run(&self) -> bool {
        self.record.run_finalizer()
    }

    /// Return the wrapped record's `OldBlockPlacement`, if any.
    /// Used by the post-sweep block reclaim path so the block
    /// the finalizer's payload still lives in stays pinned
    /// (its lines stay marked) until the drain runs.
    pub(crate) fn block_placement(&self) -> Option<OldBlockPlacement> {
        self.record.old_block_placement()
    }

    /// Apply a `(old block index -> new block index)` remap to
    /// the wrapped record's `OldBlockPlacement`. Used after
    /// empty-block reclaim renumbers the surviving blocks so
    /// the queued finalizer keeps pointing at the right slot.
    pub(crate) fn rebind_block(&mut self, new_index: usize) {
        if let Some(placement) = self.record.old_block_placement() {
            self.record.set_old_block_placement(OldBlockPlacement {
                block_index: new_index,
                ..placement
            });
        }
    }
}

#[cfg(test)]
#[path = "object_test.rs"]
mod tests;
