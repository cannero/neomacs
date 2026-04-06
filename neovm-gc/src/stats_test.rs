use super::*;
use crate::spaces::OldRegionCollectionStats;

#[test]
fn collection_stats_saturating_add_assign_accumulates_all_fields() {
    let mut total = CollectionStats {
        collections: 1,
        minor_collections: 2,
        major_collections: 3,
        pause_nanos: 4,
        reclaim_prepare_nanos: 5,
        promoted_bytes: 6,
        mark_steps: 7,
        mark_rounds: 8,
        reclaimed_bytes: 9,
        finalized_objects: 10,
        queued_finalizers: 11,
        compacted_regions: 12,
        reclaimed_regions: 13,
    };
    let delta = CollectionStats {
        collections: 10,
        minor_collections: 20,
        major_collections: 30,
        pause_nanos: 40,
        reclaim_prepare_nanos: 50,
        promoted_bytes: 60,
        mark_steps: 70,
        mark_rounds: 80,
        reclaimed_bytes: 90,
        finalized_objects: 100,
        queued_finalizers: 110,
        compacted_regions: 120,
        reclaimed_regions: 130,
    };

    total.saturating_add_assign(delta);

    assert_eq!(
        total,
        CollectionStats {
            collections: 11,
            minor_collections: 22,
            major_collections: 33,
            pause_nanos: 44,
            reclaim_prepare_nanos: 55,
            promoted_bytes: 66,
            mark_steps: 77,
            mark_rounds: 88,
            reclaimed_bytes: 99,
            finalized_objects: 110,
            queued_finalizers: 121,
            compacted_regions: 132,
            reclaimed_regions: 143,
        }
    );
}

#[test]
fn collection_stats_completed_minor_cycle_tracks_reclaim_and_regions() {
    let stats = CollectionStats::completed_minor_cycle(
        7,
        8,
        9,
        100,
        60,
        3,
        OldRegionCollectionStats {
            compacted_regions: 4,
            reclaimed_regions: 5,
        },
    );

    assert_eq!(
        stats,
        CollectionStats {
            collections: 1,
            minor_collections: 1,
            major_collections: 0,
            pause_nanos: 0,
            reclaim_prepare_nanos: 0,
            promoted_bytes: 9,
            mark_steps: 7,
            mark_rounds: 8,
            reclaimed_bytes: 40,
            finalized_objects: 0,
            queued_finalizers: 3,
            compacted_regions: 4,
            reclaimed_regions: 5,
        }
    );
}

#[test]
fn collection_stats_completed_old_gen_cycle_tracks_prepare_and_regions() {
    let stats = CollectionStats::completed_old_gen_cycle(
        11,
        12,
        13,
        14,
        120,
        70,
        6,
        OldRegionCollectionStats {
            compacted_regions: 7,
            reclaimed_regions: 8,
        },
    );

    assert_eq!(
        stats,
        CollectionStats {
            collections: 1,
            minor_collections: 0,
            major_collections: 1,
            pause_nanos: 0,
            reclaim_prepare_nanos: 14,
            promoted_bytes: 13,
            mark_steps: 11,
            mark_rounds: 12,
            reclaimed_bytes: 50,
            finalized_objects: 0,
            queued_finalizers: 6,
            compacted_regions: 7,
            reclaimed_regions: 8,
        }
    );
}

#[test]
fn heap_stats_total_live_bytes_sums_all_spaces() {
    let stats = HeapStats {
        nursery: SpaceStats {
            reserved_bytes: 99,
            live_bytes: 1,
        },
        old: SpaceStats {
            reserved_bytes: 98,
            live_bytes: 2,
        },
        pinned: SpaceStats {
            reserved_bytes: 97,
            live_bytes: 3,
        },
        large: SpaceStats {
            reserved_bytes: 96,
            live_bytes: 4,
        },
        immortal: SpaceStats {
            reserved_bytes: 95,
            live_bytes: 5,
        },
        ..HeapStats::default()
    };

    assert_eq!(stats.total_live_bytes(), 15);
}
