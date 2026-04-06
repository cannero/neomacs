use super::*;
use crate::plan::{CollectionKind, CollectionPhase};
use std::cell::RefCell;

fn major_plan() -> CollectionPlan {
    CollectionPlan {
        kind: CollectionKind::Major,
        phase: CollectionPhase::Remark,
        concurrent: true,
        parallel: true,
        worker_count: 4,
        mark_slice_budget: 8,
        target_old_regions: 2,
        selected_old_regions: vec![0, 3],
        estimated_compaction_bytes: 64,
        estimated_reclaim_bytes: 32,
    }
}

fn prepared_reclaim() -> PreparedReclaim {
    PreparedReclaim {
        promoted_bytes: 0,
        rebuilt_old_regions: Vec::new(),
        rebuilt_object_index: HashMap::new(),
        old_reserved_bytes: 0,
        old_region_stats: OldRegionCollectionStats::default(),
        survivors: Vec::new(),
        finalize_indices: Vec::new(),
        finalizable_candidates: Vec::new(),
        weak_candidates: Vec::new(),
        ephemeron_candidates: Vec::new(),
        remembered_edges: Vec::new(),
        remembered_owners: Vec::new(),
        nursery_live_bytes: 0,
        old_live_bytes: 0,
        pinned_live_bytes: 0,
        large_live_bytes: 0,
        immortal_live_bytes: 0,
    }
}

#[test]
fn prepare_major_reclaim_runs_weak_processing_before_preparing() {
    let log = RefCell::new(Vec::new());
    let reclaim = super::prepare_major_reclaim(
        &major_plan(),
        |_plan| log.borrow_mut().push("weak"),
        |_plan| {
            log.borrow_mut().push("prepare");
            prepared_reclaim()
        },
    );

    assert_eq!(&*log.borrow(), &["weak", "prepare"]);
    assert_eq!(reclaim.promoted_bytes, 0);
}

#[test]
fn prepare_full_reclaim_propagates_promotion_and_relocation_order() {
    let log = RefCell::new(Vec::new());
    let reclaim = super::prepare_full_reclaim(
        &mut (),
        &CollectionPlan {
            kind: CollectionKind::Full,
            ..major_plan()
        },
        |_heap| {
            log.borrow_mut().push("evacuate");
            Ok((41usize, 17usize))
        },
        |_heap, forwarding| {
            assert_eq!(*forwarding, 41);
            log.borrow_mut().push("relocate");
        },
        |_heap, _plan, forwarding| {
            assert_eq!(*forwarding, 41);
            log.borrow_mut().push("weak");
        },
        |_heap, _plan| {
            log.borrow_mut().push("prepare");
            prepared_reclaim()
        },
    )
    .expect("full reclaim prep should succeed");

    assert_eq!(&*log.borrow(), &["evacuate", "relocate", "weak", "prepare"]);
    assert_eq!(reclaim.promoted_bytes, 17);
}
