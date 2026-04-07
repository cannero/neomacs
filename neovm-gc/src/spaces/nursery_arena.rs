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
mod tests {
    use super::*;
    use core::alloc::Layout;

    #[test]
    fn fresh_arena_reports_empty_state() {
        let arena = NurseryArena::new(1024);
        assert_eq!(arena.capacity(), 1024);
        assert_eq!(arena.used_bytes(), 0);
        assert_eq!(arena.free_bytes(), 1024);
    }

    #[test]
    fn bump_alloc_advances_cursor_and_preserves_alignment() {
        let mut arena = NurseryArena::new(256);
        let layout = Layout::from_size_align(32, 16).unwrap();

        let ptr1 = arena.try_alloc(layout).expect("first alloc");
        let ptr2 = arena.try_alloc(layout).expect("second alloc");

        assert_ne!(ptr1.as_ptr(), ptr2.as_ptr());
        assert!(
            (ptr1.as_ptr() as usize).is_multiple_of(16),
            "first pointer is aligned"
        );
        assert!(
            (ptr2.as_ptr() as usize).is_multiple_of(16),
            "second pointer is aligned"
        );
        assert!(arena.used_bytes() >= 64);
    }

    #[test]
    fn bump_alloc_returns_none_when_arena_is_exhausted() {
        let mut arena = NurseryArena::new(64);
        let layout = Layout::from_size_align(32, 8).unwrap();
        assert!(arena.try_alloc(layout).is_some());
        assert!(arena.try_alloc(layout).is_some());
        assert!(arena.try_alloc(layout).is_none());
    }

    #[test]
    fn reset_allows_reuse_from_the_start() {
        let mut arena = NurseryArena::new(64);
        let layout = Layout::from_size_align(32, 8).unwrap();
        let first = arena.try_alloc(layout).expect("first alloc");
        assert_eq!(arena.used_bytes(), 32);
        arena.reset();
        assert_eq!(arena.used_bytes(), 0);
        let second = arena.try_alloc(layout).expect("alloc after reset");
        assert_eq!(first.as_ptr(), second.as_ptr());
    }

    #[test]
    fn contains_ptr_reports_membership() {
        let mut arena = NurseryArena::new(128);
        let layout = Layout::from_size_align(16, 8).unwrap();
        let ptr = arena.try_alloc(layout).expect("alloc").as_ptr();
        assert!(arena.contains_ptr(ptr));
        let far: *const u8 = 0xdead_beef_usize as *const u8;
        assert!(!arena.contains_ptr(far));
    }

    #[test]
    fn nursery_state_swaps_from_and_to_spaces() {
        let mut state = NurseryState::new(64);
        let layout = Layout::from_size_align(32, 8).unwrap();
        let from_ptr = state.try_alloc(layout).expect("alloc from-space");
        assert_eq!(state.from_space().used_bytes(), 32);
        let to_ptr = state
            .try_alloc_in_to_space(layout)
            .expect("alloc to-space");
        assert_eq!(state.to_space().used_bytes(), 32);
        assert!(state.from_space_contains(from_ptr.as_ptr()));
        assert!(state.contains_ptr(to_ptr.as_ptr()));

        state.swap_spaces_and_reset();
        assert_eq!(state.from_space().used_bytes(), 32);
        assert_eq!(state.to_space().used_bytes(), 0);
    }
}
