//! Bump-pointer semispace arena for nursery allocation.
//!
//! Each `NurseryArena` owns a contiguous heap buffer and services
//! allocation requests by bumping a local cursor. This replaces the
//! per-object `std::alloc::alloc` hot path for nursery objects:
//! allocation becomes a single pointer increment and dead objects are
//! reclaimed in bulk by resetting the cursor.
//!
//! `NurseryState` holds two arenas (`from_space` and `to_space`) so
//! minor collection can copy survivors between them and reset the
//! from-space in a single operation.

use core::alloc::Layout;
use core::ptr::NonNull;

/// A single bump-pointer nursery arena.
///
/// The arena owns a `Box<[u8]>` backing buffer. Allocation returns a
/// raw pointer into the buffer. Memory lifetime is managed at the
/// arena level: individual allocations are never freed; the arena is
/// reset in bulk at the end of a minor GC cycle.
#[derive(Debug)]
pub(crate) struct NurseryArena {
    buffer: Box<[u8]>,
    cursor: usize,
}

impl NurseryArena {
    /// Create an arena reserving `capacity_bytes` of bump-allocatable space.
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        let buffer: Box<[u8]> = vec![0u8; capacity_bytes].into_boxed_slice();
        Self { buffer, cursor: 0 }
    }

    /// Capacity in bytes.
    #[allow(dead_code)]
    pub(crate) fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Bytes consumed so far.
    #[allow(dead_code)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.cursor
    }

    /// Bytes still available for bump allocation.
    #[allow(dead_code)]
    pub(crate) fn free_bytes(&self) -> usize {
        self.buffer.len().saturating_sub(self.cursor)
    }

    /// Base address of the backing buffer.
    #[allow(dead_code)]
    pub(crate) fn base_ptr(&self) -> *mut u8 {
        self.buffer.as_ptr() as *mut u8
    }

    /// Reset the cursor to zero. Any prior allocations become invalid
    /// immediately — callers are responsible for draining every
    /// `ObjectRecord` backed by this arena before calling `reset`.
    pub(crate) fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Attempt to allocate a region matching `layout` from this arena.
    ///
    /// Returns `Some(ptr)` on success, where `ptr` points to a region of
    /// at least `layout.size()` bytes aligned to `layout.align()`.
    /// Returns `None` if the arena cannot satisfy the request.
    ///
    /// Safety/ownership: the returned pointer is valid for the lifetime
    /// of the arena (until `reset` is called). The caller must not free
    /// the memory with `dealloc` — the arena owns it.
    pub(crate) fn try_alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let size = layout.size();
        let align = layout.align().max(1);

        let buffer_base = self.buffer.as_ptr() as usize;
        let current = buffer_base.checked_add(self.cursor)?;
        let aligned = align_up(current, align)?;
        let padding = aligned.checked_sub(buffer_base)?;
        let end = padding.checked_add(size)?;
        if end > self.buffer.len() {
            return None;
        }

        let ptr = aligned as *mut u8;
        self.cursor = end;
        NonNull::new(ptr)
    }

    /// Returns true if `ptr` points inside this arena's backing buffer.
    #[allow(dead_code)]
    pub(crate) fn contains_ptr(&self, ptr: *const u8) -> bool {
        let base = self.buffer.as_ptr() as usize;
        let end = base.saturating_add(self.buffer.len());
        let target = ptr as usize;
        target >= base && target < end
    }
}

fn align_up(addr: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two(), "alignment must be a power of two");
    let mask = align - 1;
    addr.checked_add(mask).map(|v| v & !mask)
}

/// A bump-pointer sub-arena owned by a single evacuation worker.
///
/// During parallel minor GC the to-space buffer is partitioned into N
/// equal-sized slabs (one per worker). Each `WorkerEvacuationArena`
/// owns the right to bump-allocate inside its slab, but does NOT own
/// the underlying memory: the parent `NurseryState::to_space` arena
/// owns the buffer and reclaims it in bulk.
///
/// `WorkerEvacuationArena` is `Send` so it can be moved into a worker
/// thread, but it is intentionally not `Sync` — only the worker that
/// owns the arena bumps its cursor, eliminating contention. After all
/// workers join, the main thread calls
/// `NurseryState::merge_worker_arenas` to fold the per-worker usage
/// back into the unified to-space cursor.
#[derive(Debug)]
pub(crate) struct WorkerEvacuationArena {
    /// Base pointer of this worker's slab inside the to-space buffer.
    base: NonNull<u8>,
    /// Length of this worker's slab in bytes.
    len: usize,
    /// Bytes already bump-allocated within the slab (0..=len).
    cursor: usize,
    /// Offset of `base` from the start of the parent to-space buffer.
    /// Used by `NurseryState::merge_worker_arenas` to compute the
    /// final unified cursor.
    base_offset: usize,
}

// Safety: a `WorkerEvacuationArena` only carries raw pointers and
// indices. It must NOT outlive the parent `NurseryState::to_space`
// buffer that owns the underlying memory; that invariant is upheld by
// always producing/consuming worker arenas inside a single
// `evacuate_marked_nursery_parallel` call without escaping the scope.
unsafe impl Send for WorkerEvacuationArena {}

impl WorkerEvacuationArena {
    /// Bytes consumed so far inside this slab.
    #[allow(dead_code)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.cursor
    }

    /// Slab length in bytes.
    #[allow(dead_code)]
    pub(crate) fn capacity(&self) -> usize {
        self.len
    }

    /// Offset of this slab's base inside the parent to-space buffer.
    #[allow(dead_code)]
    pub(crate) fn base_offset(&self) -> usize {
        self.base_offset
    }

    /// Attempt to bump-allocate `layout` from this worker's slab.
    ///
    /// Returns `None` if the slab cannot satisfy the request. Callers
    /// fall back to a system allocation in that case (the same way the
    /// serial path falls back when the unified to-space cursor would
    /// overflow).
    pub(crate) fn try_alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let size = layout.size();
        let align = layout.align().max(1);

        let base_addr = self.base.as_ptr() as usize;
        let current = base_addr.checked_add(self.cursor)?;
        let aligned = align_up(current, align)?;
        let padding = aligned.checked_sub(base_addr)?;
        let end = padding.checked_add(size)?;
        if end > self.len {
            return None;
        }

        let ptr = aligned as *mut u8;
        self.cursor = end;
        NonNull::new(ptr)
    }
}

/// Semispace nursery state: a from-space arena (where current
/// allocations live) and a to-space arena (the destination for
/// survivor copies during minor GC). After copy-evacuation the two
/// arenas are swapped and the new from-space is reset to empty.
#[derive(Debug)]
pub(crate) struct NurseryState {
    from_space: NurseryArena,
    to_space: NurseryArena,
    capacity: usize,
    /// Monotonic generation counter, incremented on every
    /// [`swap_spaces_and_reset`] call. `NurseryTlab` stamps
    /// itself with the current generation at reservation time
    /// and uses the stamp to detect staleness after a minor
    /// cycle. Staleness rejects the TLAB and forces the
    /// caller to reserve a fresh one.
    generation: u64,
}

/// A per-mutator bump slab carved out of the nursery from-space.
///
/// A `NurseryTlab` reserves a contiguous range `[base, base + len)`
/// inside the from-space buffer and maintains its own bump cursor.
/// The mutator can bump-allocate within the slab without touching
/// the shared from-space cursor on every alloc: only refilling the
/// slab requires a trip through [`NurseryState::reserve_tlab`].
///
/// TLABs become stale when the nursery flips its from-space /
/// to-space via [`NurseryState::swap_spaces_and_reset`]. The
/// [`NurseryTlab::generation`] stamp is compared against the
/// current nursery generation on every alloc; a mismatch means the
/// slab no longer lives inside the active from-space and the alloc
/// fails (returning `None`) so the caller can drop the TLAB and
/// request a fresh one.
///
/// This type is the structural seam for multi-mutator nursery
/// allocation: today the crate only drives a single mutator at a
/// time, but future multi-mutator support can give each mutator
/// its own `NurseryTlab`, bump locally in the allocation hot path,
/// and coordinate only on refill/reservation. The wiring into the
/// mutator's allocation path is not yet in place; this type ships
/// as a standalone primitive with its own unit coverage so the
/// eventual wire-up has a tested foundation to build on.
#[derive(Debug)]
pub(crate) struct NurseryTlab {
    base: NonNull<u8>,
    len: usize,
    cursor: usize,
    generation: u64,
}

// Safety: `NurseryTlab` only stores a raw pointer plus
// scalars.
//
// `Send` lets a future multi-mutator refactor move a TLAB
// onto a worker thread. The invariant to uphold is the same
// as [`WorkerEvacuationArena`]: a TLAB must not outlive the
// `NurseryState::from_space` buffer it was carved from.
//
// `Sync` is sound even though the `cursor` field is
// non-atomic: every mutating method on `NurseryTlab` takes
// `&mut self`, so Rust's borrow checker already prevents
// any `&NurseryTlab` shared reference from mutating the
// cursor concurrently. The type is then only `Sync` in the
// same vacuous sense that `&mut u64` is `Sync` — there is
// no observable shared-mutable state. Adding this impl lets
// `NurseryTlab` live inside `Heap`, which is stored behind
// `Arc<RwLock<Heap>>` in `SharedHeap` and therefore
// requires `Heap: Sync`.
unsafe impl Send for NurseryTlab {}
unsafe impl Sync for NurseryTlab {}

impl NurseryTlab {
    /// Bytes remaining in this slab.
    #[allow(dead_code)]
    pub(crate) fn free_bytes(&self) -> usize {
        self.len.saturating_sub(self.cursor)
    }

    /// Bytes this slab has already bumped past.
    #[allow(dead_code)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.cursor
    }

    /// Total slab length in bytes.
    #[allow(dead_code)]
    pub(crate) fn capacity(&self) -> usize {
        self.len
    }

    /// Nursery generation stamp captured at reservation time.
    #[allow(dead_code)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Attempt to bump-allocate `layout` from this slab.
    ///
    /// `current_generation` is the current nursery generation
    /// (from [`NurseryState::generation`]). A mismatch means the
    /// slab is stale (the nursery has flipped since reservation);
    /// the call returns `None` and the caller must drop the TLAB.
    ///
    /// On success the returned pointer is valid for the lifetime
    /// of the enclosing from-space buffer (until the next
    /// swap-and-reset).
    #[allow(dead_code)]
    pub(crate) fn try_alloc(
        &mut self,
        current_generation: u64,
        layout: Layout,
    ) -> Option<NonNull<u8>> {
        if self.generation != current_generation {
            return None;
        }
        let size = layout.size();
        let align = layout.align().max(1);
        let base_addr = self.base.as_ptr() as usize;
        let current = base_addr.checked_add(self.cursor)?;
        let aligned = align_up(current, align)?;
        let padding = aligned.checked_sub(base_addr)?;
        let end = padding.checked_add(size)?;
        if end > self.len {
            return None;
        }
        let ptr = aligned as *mut u8;
        self.cursor = end;
        NonNull::new(ptr)
    }
}

// `from_space` / `to_space` are standard semispace-collector
// terminology (from-space holds live records, to-space is the
// evacuation destination), not Rust-style `from_*` conversion
// constructors. Suppress clippy::wrong_self_convention so the
// methods can keep their idiomatic GC names.
#[allow(clippy::wrong_self_convention)]
impl NurseryState {
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        Self {
            from_space: NurseryArena::new(capacity_bytes),
            to_space: NurseryArena::new(capacity_bytes),
            capacity: capacity_bytes,
            generation: 0,
        }
    }

    /// Current nursery generation counter. Incremented on every
    /// [`swap_spaces_and_reset`]. A `NurseryTlab` stamps itself
    /// with this value at reservation time to detect staleness.
    #[allow(dead_code)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Reserve a per-mutator bump slab of `size` bytes out of the
    /// from-space arena. Returns `None` if the from-space cannot
    /// service the reservation. The reserved bytes are accounted
    /// against the from-space cursor immediately, so the slab is
    /// visible to the GC as used space even before any individual
    /// allocation bumps within it.
    ///
    /// The returned [`NurseryTlab`] stamps itself with the
    /// current [`generation`] so a subsequent minor-cycle swap
    /// can invalidate the TLAB on the mutator side.
    ///
    /// `size` is rounded up to pointer alignment so the returned
    /// slab honors the minimum alignment every subsequent alloc
    /// will ask for. Callers should pass a slab size that is
    /// large enough to amortize the reservation cost against
    /// many subsequent TLAB bumps (e.g. a few kilobytes).
    #[allow(dead_code)]
    pub(crate) fn reserve_tlab(&mut self, size: usize) -> Option<NurseryTlab> {
        if size == 0 {
            return None;
        }
        let pointer_align = core::mem::align_of::<usize>().max(1);
        let layout = core::alloc::Layout::from_size_align(size, pointer_align).ok()?;
        let base = self.from_space.try_alloc(layout)?;
        Some(NurseryTlab {
            base,
            len: size,
            cursor: 0,
            generation: self.generation,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    #[allow(dead_code)]
    pub(crate) fn from_space(&self) -> &NurseryArena {
        &self.from_space
    }

    #[allow(dead_code)]
    pub(crate) fn from_space_mut(&mut self) -> &mut NurseryArena {
        &mut self.from_space
    }

    #[allow(dead_code)]
    pub(crate) fn to_space(&self) -> &NurseryArena {
        &self.to_space
    }

    #[allow(dead_code)]
    pub(crate) fn to_space_mut(&mut self) -> &mut NurseryArena {
        &mut self.to_space
    }

    /// Allocate one nursery object out of the from-space.
    pub(crate) fn try_alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.from_space.try_alloc(layout)
    }

    /// Allocate one survivor copy into the to-space during minor GC.
    pub(crate) fn try_alloc_in_to_space(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.to_space.try_alloc(layout)
    }

    /// Swap from-space and to-space, then reset the new to-space (the
    /// old from-space). Callers must have drained every record backed
    /// by the old from-space before calling this.
    ///
    /// Also increments the nursery generation counter so any
    /// outstanding [`NurseryTlab`] reserved against the
    /// pre-swap from-space becomes stale on its next alloc
    /// attempt.
    pub(crate) fn swap_spaces_and_reset(&mut self) {
        core::mem::swap(&mut self.from_space, &mut self.to_space);
        self.to_space.reset();
        self.generation = self.generation.saturating_add(1);
    }

    /// Partition the to-space into `worker_count` equal-sized slabs,
    /// returning one `WorkerEvacuationArena` per worker.
    ///
    /// The to-space cursor is left at zero. After all workers join,
    /// the caller must hand the worker arenas back to
    /// `merge_worker_arenas` so the unified cursor reflects the
    /// total bytes consumed.
    ///
    /// Slabs do not overlap. Each slab covers a contiguous range
    /// `[base + slab_index * slab_len, base + (slab_index + 1) * slab_len)`,
    /// except the last slab which absorbs any leftover bytes from
    /// integer division so the entire to-space buffer is covered.
    pub(crate) fn split_to_space_into_worker_arenas(
        &mut self,
        worker_count: usize,
    ) -> Vec<WorkerEvacuationArena> {
        let workers = worker_count.max(1);
        // Resetting the cursor here is defensive: callers always invoke
        // this on an empty to-space, but pinning to zero makes the
        // partitioning logic correct regardless.
        self.to_space.cursor = 0;
        let total = self.to_space.buffer.len();
        let base_addr = self.to_space.buffer.as_ptr() as *mut u8;

        let slab_size = total / workers;
        let mut arenas = Vec::with_capacity(workers);
        let mut offset = 0usize;
        for worker_index in 0..workers {
            let len = if worker_index + 1 == workers {
                total.saturating_sub(offset)
            } else {
                slab_size
            };
            let slab_base = unsafe { base_addr.add(offset) };
            let base = NonNull::new(slab_base).expect("non-null buffer base");
            arenas.push(WorkerEvacuationArena {
                base,
                len,
                cursor: 0,
                base_offset: offset,
            });
            offset = offset.saturating_add(len);
        }
        arenas
    }

    /// Fold the per-worker bump cursors back into the unified
    /// to-space cursor.
    ///
    /// Each worker's used bytes occupy a contiguous prefix of its
    /// slab. The unified cursor is set to the end of the last
    /// non-empty slab, computed as `slab.base_offset + slab.used_bytes`,
    /// taken across all workers. Empty slabs do not contribute. This
    /// keeps the cursor invariant: the next swap-and-reset will see a
    /// to-space whose used range covers exactly the bytes that were
    /// allocated during evacuation.
    ///
    /// Note that the unified cursor may include unused holes between
    /// slabs (e.g. if worker 0 only used 100 bytes of a 256-byte slab
    /// while worker 1 used all 256 bytes). Those holes are dead
    /// memory inside this minor cycle; they are reclaimed in bulk
    /// when the to-space is reset on the next minor GC.
    pub(crate) fn merge_worker_arenas(&mut self, arenas: &[WorkerEvacuationArena]) {
        let mut max_end = 0usize;
        for arena in arenas {
            if arena.cursor == 0 {
                continue;
            }
            let end = arena.base_offset.saturating_add(arena.cursor);
            if end > max_end {
                max_end = end;
            }
        }
        if max_end > self.to_space.buffer.len() {
            max_end = self.to_space.buffer.len();
        }
        self.to_space.cursor = max_end;
    }

    /// Returns true if `ptr` points into the from-space backing buffer.
    #[allow(dead_code)]
    pub(crate) fn from_space_contains(&self, ptr: *const u8) -> bool {
        self.from_space.contains_ptr(ptr)
    }

    /// Returns true if `ptr` points into either space's backing buffer.
    #[allow(dead_code)]
    pub(crate) fn contains_ptr(&self, ptr: *const u8) -> bool {
        self.from_space.contains_ptr(ptr) || self.to_space.contains_ptr(ptr)
    }
}

#[cfg(test)]
#[path = "nursery_arena_test.rs"]
mod tests;
