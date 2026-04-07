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
fn split_to_space_into_worker_arenas_partitions_buffer() {
    let mut state = NurseryState::new(256);
    let arenas = state.split_to_space_into_worker_arenas(4);
    assert_eq!(arenas.len(), 4);

    let total: usize = arenas.iter().map(WorkerEvacuationArena::capacity).sum();
    assert_eq!(total, 256);

    // Slabs are contiguous starting from offset 0.
    let mut expected_offset = 0usize;
    for arena in &arenas {
        assert_eq!(arena.base_offset(), expected_offset);
        expected_offset = expected_offset.saturating_add(arena.capacity());
    }
    assert_eq!(expected_offset, 256);
}

#[test]
fn split_to_space_handles_uneven_division_with_tail_slab() {
    let mut state = NurseryState::new(257);
    let arenas = state.split_to_space_into_worker_arenas(4);
    assert_eq!(arenas.len(), 4);
    // First three slabs use floor(257 / 4) = 64 bytes each.
    assert_eq!(arenas[0].capacity(), 64);
    assert_eq!(arenas[1].capacity(), 64);
    assert_eq!(arenas[2].capacity(), 64);
    // Last slab absorbs the leftover (257 - 192 = 65).
    assert_eq!(arenas[3].capacity(), 65);
    let total: usize = arenas.iter().map(WorkerEvacuationArena::capacity).sum();
    assert_eq!(total, 257);
}

#[test]
fn worker_evacuation_arena_alloc_advances_local_cursor() {
    let mut state = NurseryState::new(256);
    let mut arenas = state.split_to_space_into_worker_arenas(2);
    let layout = Layout::from_size_align(32, 8).unwrap();
    let ptr_a = arenas[0].try_alloc(layout).expect("alloc in slab 0");
    let ptr_b = arenas[0].try_alloc(layout).expect("second alloc in slab 0");
    let ptr_c = arenas[1].try_alloc(layout).expect("alloc in slab 1");
    assert_ne!(ptr_a.as_ptr(), ptr_b.as_ptr());
    assert_ne!(ptr_a.as_ptr(), ptr_c.as_ptr());
    assert_eq!(arenas[0].used_bytes(), 64);
    assert_eq!(arenas[1].used_bytes(), 32);
}

#[test]
fn worker_evacuation_arena_returns_none_when_slab_full() {
    let mut state = NurseryState::new(64);
    let mut arenas = state.split_to_space_into_worker_arenas(2);
    let layout = Layout::from_size_align(32, 8).unwrap();
    assert!(arenas[0].try_alloc(layout).is_some());
    // Each slab is 32 bytes; another 32-byte alloc no longer fits.
    assert!(arenas[0].try_alloc(layout).is_none());
    // The other slab is independent and still has room.
    assert!(arenas[1].try_alloc(layout).is_some());
}

#[test]
fn merge_worker_arenas_advances_unified_cursor_to_max_used_offset() {
    let mut state = NurseryState::new(256);
    let mut arenas = state.split_to_space_into_worker_arenas(4);
    let layout = Layout::from_size_align(16, 8).unwrap();
    // Slab 0 uses 16 bytes, slab 2 uses 32 bytes, slab 1 and 3 are empty.
    arenas[0].try_alloc(layout).unwrap();
    arenas[2].try_alloc(layout).unwrap();
    arenas[2].try_alloc(layout).unwrap();
    let snapshot = arenas;
    state.merge_worker_arenas(&snapshot);
    // Slab 2 starts at offset 128 (256/4 * 2), uses 32 bytes => end 160.
    assert_eq!(state.to_space().used_bytes(), 160);
}

#[test]
fn merge_worker_arenas_with_no_allocations_leaves_cursor_at_zero() {
    let mut state = NurseryState::new(128);
    let arenas = state.split_to_space_into_worker_arenas(4);
    state.merge_worker_arenas(&arenas);
    assert_eq!(state.to_space().used_bytes(), 0);
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
