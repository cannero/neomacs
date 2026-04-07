use super::*;
use crate::object::SpaceKind;
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

#[test]
fn prepared_heap_stats_apply_reclaim_updates_space_live_and_reserved_bytes() {
    let mut prepared = PreparedHeapStats::default();
    prepared.record_live_object(SpaceKind::Nursery, 3);
    prepared.record_live_object(SpaceKind::Old, 5);
    prepared.record_live_object(SpaceKind::Pinned, 7);
    prepared.record_live_object(SpaceKind::Large, 11);
    prepared.record_live_object(SpaceKind::Immortal, 13);
    assert_eq!(prepared.total_live_bytes(), 39);

    let mut stats = HeapStats::default();
    let after_bytes = prepared.apply_space_rebuild(&mut stats, 17);

    assert_eq!(after_bytes, 39);
    assert_eq!(stats.nursery.live_bytes, 3);
    assert_eq!(stats.old.live_bytes, 5);
    assert_eq!(stats.old.reserved_bytes, 17);
    assert_eq!(stats.pinned.live_bytes, 7);
    assert_eq!(stats.large.live_bytes, 11);
    assert_eq!(stats.large.reserved_bytes, 11);
    assert_eq!(stats.immortal.live_bytes, 13);
    assert_eq!(stats.immortal.reserved_bytes, 13);
}

#[test]
fn heap_stats_record_allocation_updates_live_and_reserved_bytes() {
    let mut stats = HeapStats::default();

    stats.record_allocation(SpaceKind::Nursery, 3, 0);
    stats.record_allocation(SpaceKind::Old, 5, 17);
    stats.record_allocation(SpaceKind::Pinned, 7, 0);
    stats.record_allocation(SpaceKind::Large, 11, 0);
    stats.record_allocation(SpaceKind::Immortal, 13, 0);

    assert_eq!(stats.nursery.live_bytes, 3);
    assert_eq!(stats.old.live_bytes, 5);
    assert_eq!(stats.old.reserved_bytes, 17);
    assert_eq!(stats.pinned.live_bytes, 7);
    assert_eq!(stats.large.live_bytes, 11);
    assert_eq!(stats.large.reserved_bytes, 11);
    assert_eq!(stats.immortal.live_bytes, 13);
    assert_eq!(stats.immortal.reserved_bytes, 13);
}
