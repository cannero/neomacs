//! Bump-pointer semispace arena for nursery allocation (Phase 1).
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
}

impl NurseryState {
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        Self {
            from_space: NurseryArena::new(capacity_bytes),
            to_space: NurseryArena::new(capacity_bytes),
            capacity: capacity_bytes,
        }
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
    pub(crate) fn swap_spaces_and_reset(&mut self) {
        core::mem::swap(&mut self.from_space, &mut self.to_space);
        self.to_space.reset();
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
