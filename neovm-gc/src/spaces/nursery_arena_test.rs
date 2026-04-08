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

#[test]
fn nursery_tlab_reserve_carves_slab_from_from_space() {
    let mut state = NurseryState::new(1024);
    let before = state.from_space().used_bytes();
    let tlab = state.reserve_tlab(256).expect("reserve tlab");
    let after = state.from_space().used_bytes();
    assert_eq!(tlab.capacity(), 256);
    assert_eq!(tlab.free_bytes(), 256);
    assert_eq!(tlab.used_bytes(), 0);
    assert!(after > before, "reservation must advance the shared cursor");
    assert_eq!(tlab.generation(), 0);
    assert_eq!(state.generation(), 0);
}

#[test]
fn nursery_tlab_try_alloc_bumps_local_cursor_without_touching_shared_cursor() {
    let mut state = NurseryState::new(1024);
    let mut tlab = state.reserve_tlab(256).expect("reserve tlab");
    let used_after_reserve = state.from_space().used_bytes();
    let generation = state.generation();

    let layout = Layout::from_size_align(32, 16).unwrap();
    let a = tlab
        .try_alloc(generation, layout)
        .expect("first tlab alloc");
    let b = tlab
        .try_alloc(generation, layout)
        .expect("second tlab alloc");
    assert_ne!(a.as_ptr(), b.as_ptr());
    assert!(
        (a.as_ptr() as usize).is_multiple_of(16),
        "tlab alloc must honor layout alignment",
    );
    assert!((b.as_ptr() as usize).is_multiple_of(16));

    // Bumps inside the slab must not change the shared
    // from-space cursor.
    assert_eq!(state.from_space().used_bytes(), used_after_reserve);
    assert!(tlab.used_bytes() >= 64);
}

#[test]
fn nursery_tlab_try_alloc_returns_none_when_slab_exhausted() {
    let mut state = NurseryState::new(1024);
    let mut tlab = state.reserve_tlab(64).expect("reserve tlab");
    let layout = Layout::from_size_align(32, 8).unwrap();
    let generation = state.generation();
    assert!(tlab.try_alloc(generation, layout).is_some());
    assert!(tlab.try_alloc(generation, layout).is_some());
    assert!(
        tlab.try_alloc(generation, layout).is_none(),
        "third alloc should overflow a 64-byte slab",
    );
}

#[test]
fn nursery_tlab_becomes_stale_after_swap_and_reset() {
    // The generation stamp must make a pre-swap TLAB reject
    // alloc attempts after the nursery flips spaces.
    let mut state = NurseryState::new(1024);
    let mut tlab = state.reserve_tlab(256).expect("reserve tlab");
    let pre_swap_generation = state.generation();
    assert_eq!(tlab.generation(), pre_swap_generation);

    state.swap_spaces_and_reset();
    let post_swap_generation = state.generation();
    assert_ne!(
        post_swap_generation, pre_swap_generation,
        "swap_spaces_and_reset must bump the generation",
    );

    let layout = Layout::from_size_align(32, 8).unwrap();
    // A post-swap alloc against the stale TLAB must fail.
    assert!(
        tlab.try_alloc(post_swap_generation, layout).is_none(),
        "stale TLAB must reject post-swap alloc",
    );
}

#[test]
fn nursery_tlab_reserve_returns_none_when_from_space_full() {
    let mut state = NurseryState::new(128);
    let _ = state.reserve_tlab(96).expect("reserve first tlab");
    // 128-byte from-space minus ~96 reserved leaves room for a
    // small second reservation but not another 96-byte slab.
    assert!(state.reserve_tlab(96).is_none());
}

#[test]
fn nursery_tlab_reserve_rejects_zero_size() {
    let mut state = NurseryState::new(1024);
    assert!(state.reserve_tlab(0).is_none());
}
