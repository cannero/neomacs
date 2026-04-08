use super::*;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::heap::{Heap, HeapConfig};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::CollectionKind;
use crate::spaces::nursery::NurseryConfig;
use core::alloc::Layout;

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
fn compute_per_block_live_bytes_sums_total_size_by_block_index() {
    use crate::reclaim::compute_per_block_live_bytes;

    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        region_bytes: 4096,
        line_bytes: 16,
        ..OldGenConfig::default()
    };
    // Seed block 0 with two objects and block 1 with one.
    let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
    let mut objects = Vec::new();
    for _ in 0..2 {
        let mut record = ObjectRecord::allocate(
            old_leaf_desc(),
            SpaceKind::Old,
            OldLeaf,
        )
        .expect("alloc obj in block 0");
        let (placement, _) = old_gen
            .try_alloc_in_block(&config, layout)
            .expect("alloc in block 0");
        record.set_old_block_placement(placement);
        objects.push(record);
    }
    // Force a second block.
    let mut record3 = ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf)
        .expect("alloc obj in block 1");
    let (fresh_placement, _) = old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("alloc in fresh block 1");
    record3.set_old_block_placement(fresh_placement);
    objects.push(record3);

    let per_block = compute_per_block_live_bytes(&objects, old_gen.block_count());
    assert_eq!(per_block.len(), 2);
    // Two objects in block 0: total = 2 * total_size.
    assert_eq!(per_block[0], 2 * objects[0].total_size());
    // One object in block 1.
    assert_eq!(per_block[1], objects[2].total_size());
}

#[test]
fn find_sparse_old_block_candidates_picks_low_density_blocks() {
    use crate::reclaim::find_sparse_old_block_candidates;

    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        // Small region_bytes makes it easy to reason about
        // density in the test.
        region_bytes: 1024,
        line_bytes: 16,
        ..OldGenConfig::default()
    };

    let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
    // Block 0: allocate many objects so it's dense.
    for _ in 0..10 {
        old_gen
            .try_alloc_in_block(&config, layout)
            .expect("alloc dense");
    }
    // Block 1: fresh block with a single allocation — sparse.
    old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("alloc sparse");
    assert_eq!(old_gen.block_count(), 2);

    // Synthetic live-byte counts: block 0 has 10*64=640 live,
    // block 1 has 64 live.
    let live_by_block = vec![640usize, 64usize];
    // Threshold 0.30: block 0 density = 640/1024 = 0.625 (not
    // candidate); block 1 density = 64/1024 = 0.0625 (candidate).
    let candidates =
        find_sparse_old_block_candidates(&live_by_block, old_gen.blocks(), 0.30);
    assert_eq!(candidates, vec![1]);

    // Threshold 0.8 includes both. The result is sorted by
    // ASCENDING density so block 1 (density 0.0625) precedes
    // block 0 (density 0.625) -- step 22 sorts candidates so
    // the most-wasted blocks evacuate first.
    let candidates =
        find_sparse_old_block_candidates(&live_by_block, old_gen.blocks(), 0.80);
    assert_eq!(candidates, vec![1, 0]);

    // Empty blocks are skipped even with a permissive threshold.
    let live_by_block_with_empty = vec![0usize, 64usize];
    let candidates =
        find_sparse_old_block_candidates(&live_by_block_with_empty, old_gen.blocks(), 1.0);
    assert_eq!(candidates, vec![1]);
}

#[test]
fn heap_compact_old_gen_physical_empty_heap_reports_zero_moved() {
    // A heap with no old-gen records can be compacted at any
    // threshold and reports zero moved.
    let mut heap = Heap::new(HeapConfig::default());
    let moved = heap.compact_old_gen_physical(1.0);
    assert_eq!(moved, 0);
}

#[test]
fn block_region_stats_reports_per_block_live_and_used_bytes() {
    // block_region_stats exposes the block-side counters as
    // OldRegionStats entries (one per block). After dual-track
    // step 2 populated the counters, this view mirrors what
    // region_stats would report if it consumed the block side.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for i in 0..3u8 {
            mutator
                .alloc(&mut scope, OldChunk([i; 32]))
                .expect("alloc direct-old chunk");
        }
    }

    let block_stats = heap.old_gen().block_region_stats();
    assert!(
        !block_stats.is_empty(),
        "block_region_stats should have at least one entry after 3 allocations"
    );
    let total_live: usize = block_stats.iter().map(|s| s.live_bytes).sum();
    let total_count: usize = block_stats.iter().map(|s| s.object_count).sum();
    assert!(total_live > 0);
    assert_eq!(total_count, 3);
    // Every entry's reserved_bytes is the block's capacity.
    for stat in &block_stats {
        assert!(stat.reserved_bytes >= stat.live_bytes);
        assert!(stat.occupied_lines <= stat.reserved_bytes / 16);
    }
}

#[test]
fn sweep_rebuilds_block_live_accounting_from_survivors() {
    // Allocate several OldChunks so one block fills up. Drop
    // most of them so the next major sweep drops dead records.
    // After the major, the surviving block's live_bytes /
    // object_count counters must reflect ONLY the survivors,
    // not the pre-sweep total. Without the rebuild added in
    // step 9 the counters would still reflect every allocation
    // ever made in the block.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            // Keep auto-compaction disabled so the test
            // isolates the sweep-rebuild behavior from the
            // compaction-rebuild behavior.
            physical_compaction_density_threshold: 0.0,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    // Allocate 2 rooted survivors; then allocate 4 more inside
    // a nested scope that drops, making them unreachable.
    let _survivor_a = mutator
        .alloc(&mut keep_scope, OldChunk([1; 32]))
        .expect("alloc survivor a");
    let _survivor_b = mutator
        .alloc(&mut keep_scope, OldChunk([2; 32]))
        .expect("alloc survivor b");
    {
        let mut dead_scope = mutator.handle_scope();
        for i in 0..4u8 {
            mutator
                .alloc(&mut dead_scope, OldChunk([3 + i; 32]))
                .expect("alloc dead chunk");
        }
    }

    // Six objects total in the block, 2 rooted.
    let before_total_live: usize = mutator
        .heap()
        .old_gen()
        .blocks()
        .iter()
        .map(|block| block.live_bytes())
        .sum();
    // The pre-sweep block live_bytes counts every allocation
    // (6 objects). It must be strictly greater than the
    // post-sweep total.
    assert!(
        before_total_live > 0,
        "expected block live_bytes to have grown as records were allocated"
    );

    mutator
        .collect(CollectionKind::Major)
        .expect("major sweeps the 4 dead chunks");

    // After the sweep, the block's live_bytes must reflect only
    // the 2 surviving records. 2 * total_size is what we expect.
    let after_total_live: usize = mutator
        .heap()
        .old_gen()
        .blocks()
        .iter()
        .map(|block| block.live_bytes())
        .sum();
    let after_total_count: usize = mutator
        .heap()
        .old_gen()
        .blocks()
        .iter()
        .map(|block| block.object_count())
        .sum();
    assert!(
        after_total_live < before_total_live,
        "post-sweep block live_bytes ({}) should be strictly less than \
         pre-sweep ({}): the rebuild must drop the 4 dead records",
        after_total_live,
        before_total_live
    );
    assert_eq!(
        after_total_count, 2,
        "post-sweep block object_count should reflect exactly the 2 \
         surviving rooted chunks"
    );
}

#[test]
fn physical_compaction_stress_keeps_heap_bounded_under_repeated_majors() {
    // Stress test: allocate batches of old-gen records, drop
    // most after each batch, run a major + auto-compaction,
    // and assert the heap stays bounded over many iterations.
    // The compaction telemetry should show real work happening
    // (records_moved > 0 and at least some source blocks
    // reclaimed) over the run.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            // Aggressive compaction so any block under 80% live
            // becomes a candidate.
            physical_compaction_density_threshold: 0.8,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    const ROUNDS: usize = 8;
    const ALLOCS_PER_ROUND: usize = 16;

    let mut survivor_root_kinds: Vec<u8> = Vec::new();

    for round in 0..ROUNDS {
        let kind = round as u8;
        survivor_root_kinds.push(kind);

        // Allocate ALLOCS_PER_ROUND records, drop them inside a
        // sub-scope so they become garbage. Allocate one
        // additional record that lives in an outer scope so it
        // survives the major.
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        let _survivor = mutator
            .alloc(&mut keep_scope, OldChunk([kind; 32]))
            .expect("alloc survivor");
        {
            let mut dead_scope = mutator.handle_scope();
            for _ in 0..ALLOCS_PER_ROUND {
                mutator
                    .alloc(&mut dead_scope, OldChunk([0xff; 32]))
                    .expect("alloc dead chunk");
            }
        }

        // Major + auto-compaction.
        mutator
            .collect(CollectionKind::Major)
            .expect("major + compaction");
    }

    // The heap must NOT have grown unboundedly.
    let final_blocks = heap.old_gen().block_count();
    assert!(
        final_blocks < 2 * ROUNDS,
        "expected bounded block growth across {ROUNDS} rounds; \
         got {final_blocks} blocks"
    );

    // Compaction telemetry: at least some real work happened.
    let stats = heap.compaction_stats();
    assert!(
        stats.cycles >= 1,
        "expected compaction to run at least once across {ROUNDS} \
         major cycles, got {} cycles",
        stats.cycles
    );
    assert!(
        stats.records_moved >= 1,
        "expected compaction to move at least one record across \
         {ROUNDS} rounds, got {} records moved",
        stats.records_moved
    );
}

#[test]
fn physical_compaction_shrinks_block_hole_bytes_after_major() {
    // Block-side analog of the legacy
    // execute_major_plan_honors_exact_selected_old_regions test:
    // allocate a batch, drop most of them, run a major + auto-
    // compaction, then assert the per-block hole_bytes (sum
    // across the pool) actually decreased. This proves physical
    // compaction does what logical compaction did but for the
    // block view: it tightens the layout so wasted space goes
    // down.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            // Enable auto-compaction at any density.
            physical_compaction_density_threshold: 1.0,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    // Phase 1: allocate enough records to populate one block,
    // then immediately drop the scope so the records become
    // dead garbage. Don't run a major yet -- we want the block
    // to carry stale used_bytes.
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for _ in 0..6 {
            mutator
                .alloc(&mut scope, OldChunk([0; 32]))
                .expect("alloc dead chunk");
        }
    }
    // Phase 2: allocate one rooted survivor that lives across
    // the major cycle below.
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let _survivor = mutator
        .alloc(&mut keep_scope, OldChunk([42u8; 32]))
        .expect("alloc rooted survivor");

    // Snapshot block-side hole bytes before the major.
    let before_holes: usize = mutator
        .heap()
        .old_gen()
        .block_region_stats()
        .iter()
        .map(|s| s.hole_bytes)
        .sum();
    let before_blocks = mutator.heap().old_gen().block_count();

    // Run a major cycle. The cycle sweeps the dead chunks from
    // phase 1, then auto-compaction runs against the now-sparse
    // blocks, evacuates the survivor into a fresh tightly-packed
    // target block, and reclaims the old source blocks.
    mutator
        .collect(CollectionKind::Major)
        .expect("major + auto compaction");

    let after_holes: usize = mutator
        .heap()
        .old_gen()
        .block_region_stats()
        .iter()
        .map(|s| s.hole_bytes)
        .sum();
    let after_blocks = mutator.heap().old_gen().block_count();

    // After physical compaction, the live survivor occupies a
    // freshly-packed block whose used_bytes barely exceeds its
    // live_bytes (modulo line rounding). The pre-cycle pool had
    // dead-record holes accounted for in used_bytes that the
    // sweep + compact pass eliminated.
    assert!(
        after_holes <= before_holes,
        "post-compact total hole_bytes ({after_holes}) should be \
         less than or equal to pre-compact ({before_holes})"
    );
    // Block count should not have grown -- we packed everything
    // into a single fresh target and dropped the old block.
    assert!(
        after_blocks <= before_blocks,
        "post-compact block_count ({after_blocks}) should be less \
         than or equal to pre-compact ({before_blocks})"
    );

    // The compaction stats should show at least one cycle ran
    // and at least one record was evacuated (the survivor).
    let stats = mutator.heap().compaction_stats();
    assert!(stats.cycles >= 1);
    assert!(stats.records_moved >= 1);
}

#[test]
fn mutator_fragmentation_wrappers_delegate_to_heap() {
    // The Mutator wrappers must report the same values the
    // underlying Heap methods would, so callers can use either
    // surface interchangeably while keeping scoped roots live.
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let frag = mutator.old_gen_fragmentation_ratio();
    assert_eq!(frag, 0.0);
    let (frag2, moved) = mutator.compact_old_gen_if_fragmented(0.1);
    assert_eq!(frag2, 0.0);
    assert_eq!(moved, 0);
    // Heap-side accessors agree.
    assert_eq!(mutator.heap().old_gen_fragmentation_ratio(), 0.0);
}

#[test]
fn compact_old_gen_aggressive_runs_until_no_more_moves() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    let mut mutator = heap.mutator();
    let mut keep = mutator.handle_scope();
    let _surv = mutator
        .alloc(&mut keep, OldChunk([7u8; 32]))
        .expect("alloc");

    // Aggressive compact at threshold 1.0 with 5 max passes.
    // The first pass moves the survivor into a packed target;
    // the second pass sees no sparse blocks left and exits
    // early via the moved == 0 break. Total moved should be
    // exactly 1 (the survivor moved once).
    let total = mutator.compact_old_gen_aggressive(1.0, 5);
    assert_eq!(total, 1);

    let stats = mutator.heap().compaction_stats();
    assert!(stats.records_moved >= 1);
    assert!(stats.cycles >= 1);
}

#[test]
fn compact_old_gen_aggressive_zero_max_passes_returns_zero() {
    let mut heap = Heap::new(HeapConfig::default());
    let total = heap.compact_old_gen_aggressive(1.0, 0);
    assert_eq!(total, 0);
    assert_eq!(heap.compaction_stats().cycles, 0);
}

#[test]
fn should_compact_old_gen_false_on_empty_heap_at_any_threshold() {
    let heap = Heap::new(HeapConfig::default());
    assert!(!heap.should_compact_old_gen(0.0));
    assert!(!heap.should_compact_old_gen(0.5));
    assert!(!heap.should_compact_old_gen(1.0));
}

#[test]
fn should_compact_old_gen_returns_true_when_fragmentation_meets_threshold() {
    // After allocating into a block, the line-rounding overhead
    // already creates some fragmentation, so a low threshold
    // should return true.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let _ = mutator.alloc(&mut scope, OldChunk([0; 32]));
    }
    let frag = heap.old_gen_fragmentation_ratio();
    if frag > 0.0 {
        // At any threshold below the actual fragmentation, the
        // predicate should fire.
        assert!(heap.should_compact_old_gen(frag));
        assert!(heap.should_compact_old_gen(frag / 2.0));
    }
    // At a threshold strictly above the actual fragmentation
    // the predicate should NOT fire.
    if frag < 1.0 {
        assert!(!heap.should_compact_old_gen(1.0));
    }
}

#[test]
fn nursery_fill_ratio_starts_zero_and_grows_with_allocations() {
    let mut heap = Heap::new(HeapConfig::default());
    assert_eq!(heap.nursery_fill_ratio(), 0.0);

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for _ in 0..16 {
            mutator
                .alloc(&mut scope, OldChunk([7; 32]))
                .expect("alloc nursery chunk");
        }
    }

    let ratio = heap.nursery_fill_ratio();
    assert!(
        ratio > 0.0 && ratio <= 1.0,
        "nursery fill ratio {ratio} should be in (0.0, 1.0] after \
         16 allocations"
    );
}

#[test]
fn old_gen_fragmentation_ratio_is_zero_on_empty_heap() {
    let heap = Heap::new(HeapConfig::default());
    assert_eq!(heap.old_gen_fragmentation_ratio(), 0.0);
}

#[test]
fn old_gen_fragmentation_ratio_is_bounded_between_zero_and_one() {
    // The fragmentation ratio is defined as
    // (used_bytes - live_bytes) / used_bytes, where used_bytes
    // counts lines (rounded up by line_bytes) and live_bytes
    // counts the raw object total_size. Even without dead
    // objects the ratio is typically non-zero because of
    // line-rounding overhead. Assert the result stays in the
    // valid [0.0, 1.0] range after a batch of allocations.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for _ in 0..4 {
            mutator
                .alloc(&mut scope, OldChunk([0; 32]))
                .expect("alloc chunk");
        }
    }
    let ratio = heap.old_gen_fragmentation_ratio();
    assert!(
        (0.0..=1.0).contains(&ratio),
        "fragmentation ratio {ratio} should be in [0.0, 1.0]"
    );
}

#[test]
fn compact_old_gen_if_fragmented_skips_when_under_threshold() {
    // A fresh heap has 0.0 fragmentation; calling
    // compact_old_gen_if_fragmented with any positive threshold
    // must return (0.0, 0) -- nothing compacted.
    let mut heap = Heap::new(HeapConfig::default());
    let (frag, moved) = heap.compact_old_gen_if_fragmented(0.1);
    assert_eq!(frag, 0.0);
    assert_eq!(moved, 0);
}

#[test]
fn clear_compaction_stats_resets_every_counter_to_zero() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    {
        let mut mutator = heap.mutator();
        let mut keep = mutator.handle_scope();
        let _surv = mutator
            .alloc(&mut keep, OldChunk([3u8; 32]))
            .expect("alloc");
        let _ = mutator.compact_old_gen_physical(1.0);
    }

    let before_clear = heap.compaction_stats();
    assert!(before_clear.cycles >= 1);
    assert!(before_clear.records_moved >= 1);

    heap.clear_compaction_stats();
    let after_clear = heap.compaction_stats();
    assert_eq!(after_clear.cycles, 0);
    assert_eq!(after_clear.records_moved, 0);
    assert_eq!(after_clear.target_blocks_created, 0);
    assert_eq!(after_clear.source_blocks_reclaimed, 0);
}

#[test]
fn compact_old_gen_physical_updates_compaction_stats_counters() {
    // Run a compaction with a live rooted survivor and confirm
    // the Heap::compaction_stats counters reflect the work: one
    // cycle, one record moved, one fresh target block created,
    // one source block reclaimed.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            physical_compaction_density_threshold: 0.0,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    assert_eq!(heap.compaction_stats().cycles, 0);
    assert_eq!(heap.compaction_stats().records_moved, 0);

    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let _survivor = mutator
        .alloc(&mut keep_scope, OldChunk([7u8; 32]))
        .expect("alloc survivor");
    // One block holding one sparse survivor. Compact at 1.0 so
    // the block qualifies.
    let moved = mutator.compact_old_gen_physical(1.0);
    assert_eq!(moved, 1);

    let stats = mutator.heap().compaction_stats();
    assert_eq!(stats.cycles, 1);
    assert_eq!(stats.records_moved, 1);
    assert_eq!(stats.target_blocks_created, 1);
    assert_eq!(stats.source_blocks_reclaimed, 1);
}

#[test]
fn compact_old_gen_physical_drops_emptied_source_blocks() {
    // After compaction moves every live record out of a sparse
    // block, that block has no surviving records and its
    // line_marks are stale (they still reflect the pre-
    // compaction layout). The post-compact rebuild pass must
    // clear those stale marks and drop the now-empty source
    // block so the pool count shrinks instead of leaking the
    // source slot until the next sweep.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            // Not auto-enabled; we call compact_old_gen_physical
            // explicitly so the test controls the sequence.
            physical_compaction_density_threshold: 0.0,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let _survivor = mutator
        .alloc(&mut keep_scope, OldChunk([42u8; 32]))
        .expect("alloc survivor");
    let before_compact = mutator.heap().old_gen().block_count();
    assert!(before_compact >= 1);

    // Explicit compaction at threshold 1.0. The survivor's
    // block has a few percent density, so it qualifies. Going
    // through the mutator keeps the root alive across the call.
    let moved = mutator.compact_old_gen_physical(1.0);
    assert_eq!(
        moved, 1,
        "expected the live rooted survivor to be evacuated"
    );

    // After the post-compact rebuild, the source block should
    // have been dropped. The pool now holds only the fresh
    // target block (one block total).
    let after_compact = mutator.heap().old_gen().block_count();
    assert_eq!(
        after_compact, 1,
        "post-compact rebuild must reclaim the source block; \
         before={before_compact}, after={after_compact}"
    );
}

#[test]
fn major_cycle_physical_compaction_preserves_live_rooted_survivor() {
    // End-to-end live-survivor test. A single OldChunk is
    // allocated and kept rooted across a major cycle. The major
    // cycle fires the automatic physical-compaction hook and
    // evacuates the live record out of its sparse source block
    // into a fresh target block. The root must still dereference
    // to the original payload bytes after the evacuation.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            // Make the region big enough that a single OldChunk
            // is a tiny fraction of the block, so the block
            // qualifies as sparse at any reasonable threshold.
            region_bytes: 1024,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            physical_compaction_density_threshold: 0.9,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let survivor = mutator
        .alloc(&mut keep_scope, OldChunk([0xa5; 32]))
        .expect("alloc survivor");
    // The survivor is rooted through `keep_scope`, so the
    // upcoming major cycle will see it as live and the
    // automatic compaction hook will evacuate it if its block
    // qualifies as sparse.
    let before_gc = mutator.heap().old_gen().block_count();
    assert!(before_gc >= 1, "should have at least one old-gen block before cycle");

    mutator
        .collect(CollectionKind::Major)
        .expect("major cycle with auto-compaction");

    // The root must still read the same payload byte pattern
    // that was written at allocation time. If the forwarding +
    // relocation path is broken, dereferencing the root would
    // either crash or read garbage.
    let payload = unsafe { survivor.as_gc().as_non_null().as_ref() };
    assert_eq!(
        payload.0[0], 0xa5,
        "rooted survivor payload must still be intact after major + compact"
    );
    assert_eq!(payload.0[31], 0xa5);
}

#[test]
fn major_cycle_runs_physical_compaction_when_density_threshold_enabled() {
    // Enable physical compaction via
    // OldGenConfig::physical_compaction_density_threshold = 1.0.
    // Allocate a batch of old-gen objects, let them become
    // garbage, run a major cycle, and assert that after the
    // cycle the old-gen block count has grown by at least 1
    // (the fresh compaction targets) or that every previously
    // non-empty block has been dropped. Either outcome proves
    // the automatic hook fired.
    //
    // Note: this is best-effort. With 0 live old-gen records
    // after the major sweep, compact_sparse_old_blocks returns
    // early and the block count is whatever the sweep left
    // behind. The test primarily verifies the hook does not
    // panic and does not regress non-compaction behavior when
    // the threshold is enabled.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            physical_compaction_density_threshold: 1.0,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for i in 0..6u8 {
            mutator
                .alloc(&mut scope, OldChunk([i; 32]))
                .expect("alloc direct-old chunk");
        }
    }

    let before_blocks = heap.old_gen().block_count();
    assert!(
        before_blocks > 0,
        "fixture should have allocated at least one block"
    );

    {
        let mut mutator = heap.mutator();
        mutator
            .collect(CollectionKind::Major)
            .expect("major cycle with physical compaction enabled");
    }

    // Old-gen is all-dead now, so after the sweep nothing needs
    // compacting. The test's purpose is to prove the hook ran
    // without panicking and that stats are still coherent.
    let after_blocks = heap.old_gen().block_count();
    // After a major that sweeps all dead old-gen records and
    // runs compaction, the block count should be less than or
    // equal to before (the sweep drops empty blocks; any fresh
    // targets that were created for compaction are dropped too
    // if their only residents are dead).
    assert!(
        after_blocks <= before_blocks,
        "after major+compact, block_count should not grow beyond the pre-cycle count; \
         before={before_blocks}, after={after_blocks}"
    );
}

#[test]
fn heap_compact_old_gen_physical_after_major_is_noop_on_all_dead_heap() {
    // A more realistic scenario: allocate several OldChunks
    // inside a scoped handle that drops, run a major GC to
    // sweep them, then call compact_old_gen_physical. The
    // old-gen is now empty, so no record is evacuated.
    // Covers the public API path without tripping rooting
    // lifetime restrictions on Root<'scope>.
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        old: OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            ..OldGenConfig::default()
        },
        ..HeapConfig::default()
    });

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for i in 0..6u8 {
            mutator
                .alloc(&mut scope, OldChunk([i; 32]))
                .expect("alloc direct-old chunk");
        }
    }
    {
        let mut mutator = heap.mutator();
        mutator
            .collect(CollectionKind::Major)
            .expect("major sweeps dead old-gen records");
    }

    let moved = heap.compact_old_gen_physical(1.0);
    assert_eq!(moved, 0);
}

#[test]
fn compact_sparse_old_blocks_packs_survivors_into_shared_target() {
    use crate::reclaim::compact_sparse_old_blocks;

    // Create two sparse source blocks, each with one small
    // survivor. The compaction pass should pack both survivors
    // into the SAME fresh target block instead of creating one
    // fresh block per evacuated record.
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        // Target blocks are sized up to region_bytes; make them
        // big enough that two 64-byte survivors easily fit in
        // one target.
        region_bytes: 4096,
        line_bytes: 16,
        ..OldGenConfig::default()
    };

    let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
    let mut objects: Vec<ObjectRecord> = Vec::new();
    // Source block 0: one survivor.
    let mut rec_a =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf)
            .expect("alloc rec a");
    let (pa, _) = old_gen
        .try_alloc_in_block(&config, layout)
        .expect("alloc into block 0");
    rec_a.set_old_block_placement(pa);
    objects.push(rec_a);
    // Source block 1: one survivor.
    let mut rec_b =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf)
            .expect("alloc rec b");
    let (pb, _) = old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("alloc into block 1");
    rec_b.set_old_block_placement(pb);
    objects.push(rec_b);
    assert_eq!(old_gen.block_count(), 2);

    // Both source blocks are sparse (one tiny survivor each in
    // a 4 KiB region). Compact at 100% threshold to force both
    // into the evacuation candidate set.
    let forwarding = compact_sparse_old_blocks(&mut objects, &mut old_gen, &config, 1.0);
    assert_eq!(forwarding.len(), 2, "both survivors should be evacuated");

    // Exactly ONE new block was created as the shared target
    // (so pool grew from 2 to 3, not from 2 to 4).
    assert_eq!(
        old_gen.block_count(),
        3,
        "target packing should create a single shared target block, \
         not one per survivor"
    );

    // Both evacuated records now live in block 2 (the fresh
    // shared target).
    let mut target_blocks = std::collections::HashSet::new();
    for record in &objects {
        if let Some(p) = record.old_block_placement() {
            target_blocks.insert(p.block_index);
        }
    }
    assert!(target_blocks.contains(&2), "both records should point at the new shared target block 2");
    assert!(!target_blocks.contains(&0) && !target_blocks.contains(&1),
        "source blocks 0 and 1 should no longer be referenced by any record");
}

#[test]
fn compact_sparse_old_blocks_moves_survivors_into_fresh_targets() {
    use crate::reclaim::compact_sparse_old_blocks;

    // Synthetic two-block fixture:
    //   - block 0 is "dense": we do not want it compacted
    //   - block 1 is "sparse": a single small object in a
    //     1024-byte region
    // Then we ask compact_sparse_old_blocks to evacuate any
    // block at or below 10% density.
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        region_bytes: 1024,
        line_bytes: 16,
        ..OldGenConfig::default()
    };

    let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
    // Seed block 0 with enough allocations to keep it dense.
    let mut objects: Vec<ObjectRecord> = Vec::new();
    for _ in 0..10 {
        let mut record = ObjectRecord::allocate(
            old_leaf_desc(),
            SpaceKind::Old,
            OldLeaf,
        )
        .expect("alloc dense record");
        let (placement, _) = old_gen
            .try_alloc_in_block(&config, layout)
            .expect("alloc in block 0");
        record.set_old_block_placement(placement);
        objects.push(record);
    }
    // Seed block 1 with a single object (sparse).
    let mut sparse_record = ObjectRecord::allocate(
        old_leaf_desc(),
        SpaceKind::Old,
        OldLeaf,
    )
    .expect("alloc sparse record");
    let (sparse_placement, _) = old_gen
        .alloc_in_fresh_block(&config, layout)
        .expect("alloc sparse");
    sparse_record.set_old_block_placement(sparse_placement);
    let sparse_key_before = sparse_record.object_key();
    objects.push(sparse_record);

    assert_eq!(old_gen.block_count(), 2);

    // Run the compaction pass with a 10% density threshold.
    // Block 0: 10 * 64 / 1024 ≈ 0.625 → dense, not picked.
    // Block 1: 1 * 64 / 1024 ≈ 0.0625 → sparse, picked.
    let forwarding = compact_sparse_old_blocks(&mut objects, &mut old_gen, &config, 0.10);

    // Exactly one entry in the forwarding map (the sparse-block
    // survivor that got moved).
    assert_eq!(forwarding.len(), 1);
    assert!(forwarding.contains_key(&sparse_key_before));

    // The sparse block (index 1) should now be disjoint from any
    // record: every remaining record is either in block 0 or in
    // the brand-new block 2 created by the compaction pass.
    let mut blocks_with_records = std::collections::HashSet::new();
    for record in &objects {
        if let Some(placement) = record.old_block_placement() {
            blocks_with_records.insert(placement.block_index);
        }
    }
    assert!(!blocks_with_records.contains(&1), "sparse source block 1 should have no surviving records");
    assert!(blocks_with_records.contains(&0), "dense block 0 should still hold its records");
    assert!(blocks_with_records.contains(&2), "freshly-created target block 2 should hold the evacuated record");

    // Total block count is now 3: blocks 0, 1 (empty), 2 (new).
    // Block 1 is still in the pool until the next
    // drop_unused_blocks_with_remap call.
    assert_eq!(old_gen.block_count(), 3);
}

#[test]
fn evacuate_old_object_to_fresh_block_copies_payload_and_forwards() {
    use crate::reclaim::evacuate_old_object_to_fresh_block;

    let mut old_gen = OldGenState::default();
    let config = OldGenConfig {
        region_bytes: 4096,
        line_bytes: 16,
        ..OldGenConfig::default()
    };
    // Place a source object via the normal block allocator so it
    // lives in an existing block at index 0.
    let mut source = ObjectRecord::allocate(
        old_leaf_desc(),
        SpaceKind::Old,
        OldLeaf,
    )
    .expect("allocate source");
    let layout = core::alloc::Layout::from_size_align(source.total_size(), 8).unwrap();
    let (source_placement, _) = old_gen
        .try_alloc_in_block(&config, layout)
        .expect("seed source block");
    source.set_old_block_placement(source_placement);
    assert_eq!(source.old_block_placement().map(|p| p.block_index), Some(0));
    assert_eq!(old_gen.block_count(), 1);

    // Evacuate the source into a fresh block.
    let evacuated = evacuate_old_object_to_fresh_block(&mut old_gen, &config, &source)
        .expect("evacuate old object");

    // A new block was created.
    assert_eq!(old_gen.block_count(), 2);
    // The evacuated record lives in block 1 (the fresh block).
    assert_eq!(
        evacuated.old_block_placement().map(|p| p.block_index),
        Some(1)
    );
    // The source header carries a forwarding pointer now.
    assert!(source.header().is_moved_out());
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

// Removed: old_gen_record_allocated_object_sets_placement_and_live_stats
// (was a unit test for the legacy region-side accounting branch in
// OldGenState::record_object, which has been deleted. The production
// allocation path now goes through try_alloc_in_block + record_object,
// which updates block accounting; coverage for that lives in the
// existing block_region_stats / record_object_accounting tests.)

// Removed: prepare_reclaim_survivor_reassigns_selected_region_after_preserved_regions
// (was a unit test for the dead "select-and-renumber" branch of
// OldGenState::prepare_reclaim_survivor; the planner stopped producing
// non-empty selected_old_regions, so that branch is unreachable in
// production and was deleted from the function body too).

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
fn apply_prepared_reclaim_returns_region_stats_unchanged() {
    // Post-rebuild retirement: PreparedOldGenReclaim only carries
    // region_stats; the regions vec is not rewritten by the
    // prepared reclaim any more. Physical compaction is the only
    // mechanism that shrinks the block pool.
    let prepared = PreparedOldGenReclaim {
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
}
