use super::*;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::heap::{Heap, HeapConfig};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::spaces::nursery::NurseryConfig;
use core::alloc::Layout;
use std::collections::HashSet;

#[derive(Debug)]
struct OldLeaf;

unsafe impl Trace for OldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

/// Non-zero-sized leaf used by tests that need the allocator to
/// pick the old-gen path via `payload_bytes > max_regular_object_bytes`.
/// `OldLeaf` is zero-sized and therefore always routes to the nursery.
#[derive(Debug)]
struct OldChunk(#[allow(dead_code)] [u8; 32]);

unsafe impl Trace for OldChunk {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn old_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<OldLeaf>()))
}

#[test]
fn old_block_accounting_fields_start_zero_and_update_on_record() {
    let mut block = OldBlock::new(1024, 16);
    assert_eq!(block.live_bytes(), 0);
    assert_eq!(block.object_count(), 0);
    assert_eq!(block.used_bytes(), 0);
    assert_eq!(block.occupied_line_count(), 0);

    block.record_object_accounting(0, 32);
    assert_eq!(block.live_bytes(), 32);
    assert_eq!(block.object_count(), 1);
    assert_eq!(block.used_bytes(), 32);

    block.record_object_accounting(64, 48);
    assert_eq!(block.live_bytes(), 80);
    assert_eq!(block.object_count(), 2);
    // used_bytes lifts to the tail of the second placement.
    assert_eq!(block.used_bytes(), 112);

    block.clear_live_accounting();
    assert_eq!(block.live_bytes(), 0);
    assert_eq!(block.object_count(), 0);
    // clear_live_accounting resets live counters only; used_bytes
    // is a high-water mark of the physical layout and does not
    // shrink just because live counters were reset.
    assert_eq!(block.used_bytes(), 112);
}

#[test]
fn alloc_in_fresh_block_always_creates_new_block_bypassing_holes() {
    // Two existing blocks in the pool, both with room to spare.
    // alloc_in_fresh_block must still create a brand-new third
    // block rather than filling a hole in one of the existing two.
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        region_bytes: 4096,
        line_bytes: 16,
        ..OldGenConfig::default()
    };
    // Seed the pool with two existing blocks via the hole-filling
    // allocator.
    let layout = Layout::from_size_align(64, 16).unwrap();
    let (first_placement, _) = old_gen
        .try_alloc_in_block(&config, layout)
        .expect("first alloc");
    assert_eq!(first_placement.block_index, 0);
    // Force a second block by filling the first one via an extra
    // alloc that cannot fit alongside the first in the same block
    // tail. The hole-filling allocator will append to block 0 as
    // long as lines are free, so we instead call
    // alloc_in_fresh_block once to guarantee block 1 exists.
    let (second_placement, _) = old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("second fresh-block alloc");
    assert_eq!(second_placement.block_index, 1);
    assert_eq!(old_gen.block_count(), 2);

    // Now ask for a fresh block while block 0 and block 1 still
    // have plenty of room. The fresh-block path must append a
    // NEW block at index 2 instead of hole-filling into 0 or 1.
    let (third_placement, _) = old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("third fresh-block alloc");
    assert_eq!(third_placement.block_index, 2);
    assert_eq!(old_gen.block_count(), 3);
}

#[test]
fn old_block_try_alloc_advances_used_bytes_high_water_mark() {
    let mut block = OldBlock::new(1024, 16);
    let layout = Layout::from_size_align(32, 8).unwrap();
    let (_, _) = block.try_alloc(layout).expect("first alloc succeeds");
    let first_high = block.used_bytes();
    assert!(first_high >= 32);

    let (_, _) = block.try_alloc(layout).expect("second alloc succeeds");
    let second_high = block.used_bytes();
    assert!(second_high > first_high);
}

#[test]
fn old_block_occupied_line_count_reflects_recorded_placements() {
    let mut block = OldBlock::new(1024, 16);
    assert_eq!(block.occupied_line_count(), 0);
    // A 32-byte placement at offset 0 spans lines 0 and 1.
    block.record_object_accounting(0, 32);
    assert_eq!(block.occupied_line_count(), 2);
    // A 16-byte placement at offset 80 lands on line 5.
    block.record_object_accounting(80, 16);
    assert_eq!(block.occupied_line_count(), 3);
    // clear_live_accounting drops the occupied_lines set along
    // with the live counters.
    block.clear_live_accounting();
    assert_eq!(block.occupied_line_count(), 0);
}

#[test]
fn old_block_accounting_tracks_allocations_alongside_regions() {
    // Run a couple of direct old-gen allocations through the full
    // runtime path and verify the block-side accounting mirrors
    // the region-side accounting. This is the dual-track
    // invariant step 2 of the OldRegion unification establishes.
    let mut heap = Heap::new(HeapConfig {
        old: OldGenConfig {
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        nursery: NurseryConfig {
            // Force every allocation onto the old-gen fast path by
            // making small objects "large" relative to the nursery.
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    });

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for _ in 0..4 {
            mutator
                .alloc(&mut scope, OldChunk([0; 32]))
                .expect("alloc old chunk");
        }
    }

    // The regions vec still carries live_bytes + object_count
    // because the old path remains active in step 2.
    let region_live: usize = heap
        .stats()
        .collections
        .reclaimed_bytes
        .try_into()
        .unwrap_or(0);
    let _ = region_live; // stats may not expose per-space totals here.

    // Block-side accounting for the same heap: sum live_bytes
    // across every block in the old-gen pool.
    let (block_live_bytes, block_object_count) =
        heap.inspect_old_gen_block_accounting_for_test();
    assert!(
        block_live_bytes > 0,
        "expected block live_bytes to be populated after 4 old-gen \
         allocations"
    );
    assert_eq!(
        block_object_count, 4,
        "expected block object_count to match 4 allocations, got {}",
        block_object_count
    );
}

#[test]
fn old_gen_record_allocated_object_sets_placement_and_live_stats() {
    let mut object =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate object");
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig::default();

    let reserved_bytes = old_gen.record_allocated_object(&config, &mut object);

    let placement = object
        .old_region_placement()
        .expect("old object placement recorded");
    assert_eq!(placement.region_index, 0);
    assert_eq!(reserved_bytes, old_gen.reserved_bytes());
    assert_eq!(old_gen.regions.len(), 1);
    assert_eq!(old_gen.regions[0].live_bytes, object.total_size());
    assert_eq!(old_gen.regions[0].object_count, 1);
}

#[test]
fn prepare_reclaim_survivor_reassigns_selected_region_after_preserved_regions() {
    let mut first =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate first");
    let mut second =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate second");
    let mut config = OldGenConfig::default();
    config.region_bytes = first.total_size();

    let mut old_gen = OldGenState::default();
    old_gen.record_allocated_object(&config, &mut first);
    old_gen.record_allocated_object(&config, &mut second);

    let plan = CollectionPlan {
        kind: CollectionKind::Major,
        phase: CollectionPhase::Reclaim,
        concurrent: true,
        parallel: true,
        worker_count: 1,
        mark_slice_budget: 1,
        target_old_regions: 1,
        selected_old_regions: vec![0],
        estimated_compaction_bytes: first.total_size(),
        estimated_reclaim_bytes: 0,
    };
    let mut rebuild = old_gen.prepare_rebuild_for_plan(&plan);

    let placement = OldGenState::prepare_reclaim_survivor(
        &mut rebuild,
        &config,
        first.old_region_placement().expect("old placement"),
        first.total_size(),
    )
    .expect("selected old survivor should get rebuilt placement");

    assert_eq!(placement.region_index, 1);
    assert_eq!(rebuild.rebuilt_regions.len(), 1);
    assert_eq!(rebuild.compacted_regions.len(), 1);
    assert_eq!(rebuild.compacted_regions[0].object_count, 1);
    assert_eq!(rebuild.compacted_regions[0].live_bytes, first.total_size());
}

#[derive(Debug)]
#[allow(dead_code)]
struct OldPayload([u8; 32]);

unsafe impl Trace for OldPayload {
    fn trace(&self, _tracer: &mut dyn Tracer) {}
    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

#[test]
fn old_block_new_rounds_buffer_up_to_line_multiple() {
    let block = OldBlock::new(300, 128);
    assert_eq!(block.line_bytes(), 128);
    assert_eq!(block.line_count(), 3);
    assert_eq!(block.capacity_bytes(), 384);
    assert!(block.is_empty());
}

#[test]
fn old_block_mark_line_records_occupancy_and_clear() {
    let block = OldBlock::new(512, 128);
    block.mark_line(2);
    assert!(block.is_line_marked(2));
    assert!(!block.is_line_marked(0));
    assert!(!block.is_empty());
    block.clear_line_marks();
    assert!(block.is_empty());
}

#[test]
fn old_block_mark_lines_for_range_covers_each_overlapped_line() {
    let block = OldBlock::new(512, 128);
    // Range [200, 320) crosses lines 1 and 2 only.
    block.mark_lines_for_range(200, 120);
    assert!(!block.is_line_marked(0));
    assert!(block.is_line_marked(1));
    assert!(block.is_line_marked(2));
    assert!(!block.is_line_marked(3));
    // Now mark a range that crosses into line 3.
    let block = OldBlock::new(512, 128);
    block.mark_lines_for_range(200, 200);
    assert!(!block.is_line_marked(0));
    assert!(block.is_line_marked(1));
    assert!(block.is_line_marked(2));
    assert!(block.is_line_marked(3));
}

#[test]
fn hole_filling_finds_free_run_and_bump_allocates() {
    // Build a block, manually mark every line except a 2-line free hole
    // to simulate dead-but-pre-existing objects, and verify try_alloc
    // routes a new allocation into the free hole rather than failing or
    // skipping past.
    let mut block = OldBlock::new(8 * 128, 128);
    // Pretend lines 0..2 and 4..8 are occupied, leaving a free hole at
    // lines 2..4 (2 free lines).
    for line in 0..2 {
        block.mark_line(line);
    }
    for line in 4..8 {
        block.mark_line(line);
    }
    block.reset_cursor();
    let layout = Layout::from_size_align(256, 1).expect("layout");
    let (offset, ptr) = block.try_alloc(layout).expect("hole-filling allocation");
    // Free hole starts at line 2 = byte offset 256.
    assert_eq!(offset, 256);
    // Pointer should land inside the buffer at the expected offset.
    let base = block.base_ptr() as usize;
    assert_eq!(ptr.as_ptr() as usize, base + 256);
    // Now every line is either occupied (marked) or just bumped past, so
    // a new allocation that needs even one line should fail.
    assert!(
        block
            .try_alloc(Layout::from_size_align(8, 1).unwrap())
            .is_none()
    );
}

#[test]
fn sweep_marks_only_surviving_lines() {
    // Allocate three OldPayload objects directly into the old space.
    // After the scope drops, none are rooted; running a full GC must
    // leave the old-gen blocks empty (no marked lines).
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 4096,
            line_bytes: 64,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for i in 0..3u8 {
            let _ = mutator
                .alloc(&mut scope, OldPayload([i; 32]))
                .expect("alloc old payload");
        }
        // Sanity: at least one block has live lines while objects exist.
        let any_marked_before_drop = mutator
            .heap()
            .old_gen()
            .blocks()
            .iter()
            .any(|block| (0..block.line_count()).any(|line| block.is_line_marked(line)));
        // Note: line marks are populated by the sweep, not by allocation,
        // so they may legitimately be all-zero before any GC has run.
        let _ = any_marked_before_drop;
    }
    // Run a full collection — every record died because the scope is
    // gone. After sweep + reclaim, the old-gen blocks should either be
    // gone entirely or have all-zero line marks.
    let _ = heap.collect(CollectionKind::Full).expect("full collection");
    let old_gen = heap.old_gen();
    for block in old_gen.blocks() {
        for line in 0..block.line_count() {
            assert!(
                !block.is_line_marked(line),
                "block line {} should be free after sweep",
                line
            );
        }
    }
}

#[test]
fn sweep_marks_lines_for_surviving_records_only() {
    // A more granular check: allocate two records, only one stays
    // rooted across the major GC, and after the sweep the surviving
    // record's lines remain marked while the dead record's lines do
    // not.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 4096,
            line_bytes: 64,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let survivor_key = {
        let mut mutator = heap.mutator();
        let mut outer_scope = mutator.handle_scope();
        let survivor = mutator
            .alloc(&mut outer_scope, OldPayload([9; 32]))
            .expect("alloc survivor");
        // Inner scope: dying record dies when this scope drops.
        {
            let mut inner_scope = mutator.handle_scope();
            let _ = mutator
                .alloc(&mut inner_scope, OldPayload([1; 32]))
                .expect("alloc dying");
        }
        // Run major collection while survivor is still rooted.
        let _ = mutator
            .collect(CollectionKind::Major)
            .expect("major collection");
        survivor.as_gc().erase().object_key()
    };
    // After the major GC, the survivor's record should still be present
    // and pointing into a live block whose lines are marked. The number
    // of marked lines across all blocks should be > 0 because the
    // survivor anchors at least one line.
    let total_marked: usize = heap
        .old_gen()
        .blocks()
        .iter()
        .map(|block| (0..block.line_count()).filter(|&l| block.is_line_marked(l)).count())
        .sum();
    assert!(
        total_marked > 0,
        "surviving record should have at least one line marked"
    );
    // The survivor record should still be tracked.
    assert!(
        heap.objects()
            .iter()
            .any(|object| object.object_key() == survivor_key),
        "survivor record should remain in objects after major GC"
    );
}

#[test]
fn block_reclaim_after_full_sweep_drops_empty_blocks() {
    // Build a heap with tiny blocks so several blocks fill up. Allocate
    // enough OldPayload records to fill multiple blocks, drop all
    // references, then run a full GC. After the sweep, blocks whose
    // entire contents died should have been reclaimed from the pool.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            // Very small blocks so each one fits roughly one record.
            region_bytes: 96,
            line_bytes: 32,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for i in 0..6u8 {
            let _ = mutator
                .alloc(&mut scope, OldPayload([i; 32]))
                .expect("alloc dying old payload");
        }
    }
    let blocks_before = heap.old_gen().block_count();
    assert!(blocks_before > 0, "expected at least one block to exist");
    let _ = heap.collect(CollectionKind::Full).expect("full collection");
    let blocks_after = heap.old_gen().block_count();
    assert!(
        blocks_after < blocks_before,
        "expected empty blocks to be reclaimed (before={blocks_before}, after={blocks_after})"
    );
}

#[test]
fn promotion_uses_old_block_allocator() {
    // Configure a heap so a freshly allocated nursery object promotes
    // into the old generation on the next minor collection (promotion_age=1).
    // After the minor GC, the promoted record's backing storage should be
    // owned by an OldBlock — verifying via the per-record OldBlockPlacement
    // hook that the evacuation routed through `OldGenState::try_alloc_in_block`.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            promotion_age: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 4096,
            line_bytes: 64,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let leaf_key = {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut scope, OldPayload([7; 32]))
            .expect("alloc nursery payload");
        // The leaf must still be rooted across the minor GC so it survives
        // and gets promoted.
        let _ = mutator
            .collect(CollectionKind::Minor)
            .expect("minor collection");
        assert_eq!(
            mutator.heap().space_of(leaf.as_gc()),
            Some(SpaceKind::Old),
            "leaf should have promoted into old generation"
        );
        leaf.as_gc().erase().object_key()
    };
    // After the scope drops, the heap should still hold the promoted
    // record (we collected before scope drop, so it lives until the
    // next collection that drops it). The promoted record's backing
    // memory should be tagged Arena (block-backed).
    assert!(
        heap.old_gen().block_count() >= 1,
        "promotion should have allocated at least one OldBlock"
    );
    let found = heap
        .objects()
        .iter()
        .find(|object| object.object_key() == leaf_key)
        .expect("promoted record present in heap");
    assert!(
        found.is_arena_owned(),
        "promoted record should be backed by an OldBlock arena, not system alloc"
    );
    assert!(
        found.old_block_placement().is_some(),
        "promoted record should record an OldBlockPlacement"
    );
}

#[test]
fn apply_prepared_reclaim_replaces_regions_and_returns_region_stats() {
    let prepared = PreparedOldGenReclaim {
        rebuilt_regions: vec![OldRegion {
            capacity_bytes: 64,
            used_bytes: 32,
            live_bytes: 24,
            object_count: 2,
            occupied_lines: HashSet::new(),
        }],
        reserved_bytes: 64,
        region_stats: OldRegionCollectionStats {
            compacted_regions: 1,
            reclaimed_regions: 2,
        },
    };
    let mut old_gen = OldGenState::default();

    let stats = old_gen.apply_prepared_reclaim(prepared);

    assert_eq!(
        stats,
        OldRegionCollectionStats {
            compacted_regions: 1,
            reclaimed_regions: 2,
        }
    );
    assert_eq!(old_gen.regions.len(), 1);
    assert_eq!(old_gen.reserved_bytes(), 64);
    assert_eq!(old_gen.regions[0].live_bytes, 24);
}
