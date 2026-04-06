use super::*;
use crate::mark::MarkWorklist;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};

fn major_plan() -> CollectionPlan {
    CollectionPlan {
        kind: CollectionKind::Major,
        phase: CollectionPhase::ConcurrentMark,
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

fn full_plan() -> CollectionPlan {
    CollectionPlan {
        kind: CollectionKind::Full,
        ..major_plan()
    }
}

fn prepared_major_reclaim() -> PreparedMajorReclaim {
    PreparedMajorReclaim {
        old_region_placements: std::collections::HashMap::new(),
        rebuilt_old_regions: Vec::new(),
        old_region_stats: OldRegionCollectionStats {
            compacted_regions: 1,
            reclaimed_regions: 0,
        },
        survivor_count: 0,
        weak_candidates: Vec::new(),
        ephemeron_candidates: Vec::new(),
        remembered_edges: Vec::new(),
        nursery_live_bytes: 0,
        old_live_bytes: 0,
        pinned_live_bytes: 0,
        large_live_bytes: 0,
        immortal_live_bytes: 0,
    }
}

#[test]
fn enqueue_active_major_mark_index_requires_active_session() {
    let mut state = CollectorState::default();

    assert!(!state.enqueue_active_major_mark_index(3));
}

#[test]
fn enqueue_active_major_mark_index_updates_remaining_work() {
    let mut state = CollectorState::default();
    let mut worklist = MarkWorklist::default();
    worklist.push(1usize);
    state.begin_major_mark(major_plan(), worklist);

    assert!(state.enqueue_active_major_mark_index(7));

    let progress = state
        .major_mark_progress()
        .expect("active major-mark progress");
    assert!(!progress.completed);
    assert_eq!(progress.remaining_work, 2);
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::ConcurrentMark
    );
}

#[test]
fn update_active_major_mark_switches_phase_to_remark_when_drained() {
    let mut state = CollectorState::default();
    let mut worklist = MarkWorklist::default();
    worklist.push(5usize);
    state.begin_major_mark(major_plan(), worklist);

    let progress = state
        .update_active_major_mark(|plan, mut worklist| {
            assert_eq!(plan.kind, CollectionKind::Major);
            assert_eq!(worklist.pop(), Some(5));
            MajorMarkUpdate {
                worklist,
                drained_objects: 1,
                mark_steps_delta: 1,
                mark_rounds_delta: 1,
            }
        })
        .expect("mark update should succeed");

    assert!(progress.completed);
    assert_eq!(progress.remaining_work, 0);
    assert_eq!(progress.mark_steps, 1);
    assert_eq!(progress.mark_rounds, 1);

    let snapshot = state.shared_snapshot();
    assert_eq!(
        snapshot
            .active_major_mark_plan
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Remark
    );
    assert_eq!(
        snapshot
            .major_mark_progress
            .expect("active major-mark progress")
            .remaining_work,
        0
    );
}

#[test]
fn major_ready_requires_reclaim_prep_after_worklist_drains() {
    let mut state = CollectorState::default();
    let mut worklist = MarkWorklist::default();
    worklist.push(5usize);
    state.begin_major_mark(major_plan(), worklist);

    let progress = state
        .update_active_major_mark(|_plan, mut worklist| MajorMarkUpdate {
            drained_objects: usize::from(worklist.pop().is_some()),
            worklist,
            mark_steps_delta: 1,
            mark_rounds_delta: 1,
        })
        .expect("mark update should succeed");

    assert!(progress.completed);
    assert!(!state.active_major_mark_is_ready());
    assert!(!state.active_major_mark_reclaim_prepared());
    assert_eq!(
        state
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Remark
    );

    assert!(state.complete_active_major_reclaim_prep(2, 3, prepared_major_reclaim()));
    assert!(state.active_major_mark_is_ready());
    assert!(state.active_major_mark_reclaim_prepared());
    assert!(state.active_major_mark_has_prepared_reclaim());
    assert_eq!(
        state
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Reclaim
    );
    let progress = state
        .major_mark_progress()
        .expect("active major-mark progress");
    assert_eq!(progress.mark_steps, 3);
    assert_eq!(progress.mark_rounds, 4);

    assert!(state.enqueue_active_major_mark_index(9));
    assert!(!state.active_major_mark_is_ready());
    assert!(!state.active_major_mark_reclaim_prepared());
    assert!(!state.active_major_mark_has_prepared_reclaim());
    assert_eq!(
        state
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::ConcurrentMark
    );
}

#[test]
fn full_ready_can_record_remark_prep_without_reclaim_prep() {
    let mut state = CollectorState::default();
    let mut worklist = MarkWorklist::default();
    worklist.push(7usize);
    state.begin_major_mark(full_plan(), worklist);

    let progress = state
        .update_active_major_mark(|_plan, mut worklist| MajorMarkUpdate {
            drained_objects: usize::from(worklist.pop().is_some()),
            worklist,
            mark_steps_delta: 1,
            mark_rounds_delta: 1,
        })
        .expect("mark update should succeed");

    assert!(progress.completed);
    assert!(state.active_major_mark_is_ready());
    assert!(!state.active_major_mark_ephemerons_processed());
    assert!(!state.active_major_mark_reclaim_prepared());

    assert!(state.complete_active_major_remark(2, 3));
    assert!(state.active_major_mark_is_ready());
    assert!(state.active_major_mark_ephemerons_processed());
    assert!(!state.active_major_mark_reclaim_prepared());
    assert_eq!(
        state
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Remark
    );
    let progress = state
        .major_mark_progress()
        .expect("active major-mark progress");
    assert_eq!(progress.mark_steps, 3);
    assert_eq!(progress.mark_rounds, 4);
}
