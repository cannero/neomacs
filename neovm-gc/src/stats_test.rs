use super::*;

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
