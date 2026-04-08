use super::*;
use crate::descriptor::{Relocator, Trace, Tracer, fixed_type_desc};
use crate::index_state::{ObjectIndex, PreparedIndexReclaim};
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::reclaim::PreparedReclaimSurvivor;
use crate::spaces::{OldGenConfig, OldGenState, OldRegionCollectionStats, PreparedOldGenReclaim};
use crate::stats::{HeapStats, PreparedHeapStats};
use std::sync::TryLockError;
use std::time::Duration;

fn major_plan() -> CollectionPlan {
    CollectionPlan {
        kind: CollectionKind::Major,
        phase: CollectionPhase::ConcurrentMark,
        concurrent: true,
        parallel: true,
        worker_count: 4,
        mark_slice_budget: 8,
        target_old_regions: 2,
        selected_old_blocks: vec![0, 3],
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

fn prepared_reclaim() -> PreparedReclaim {
    PreparedReclaim {
        promoted_bytes: 0,
        old_gen: PreparedOldGenReclaim {
            region_stats: OldRegionCollectionStats {
                compacted_regions: 1,
                reclaimed_regions: 0,
            },
        },
        indexes: PreparedIndexReclaim::default(),
        survivors: vec![PreparedReclaimSurvivor { object_index: 0 }],
        stats: PreparedHeapStats::default(),
    }
}

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
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

    assert!(state.complete_active_major_reclaim_prep(
        2,
        3,
        Duration::from_nanos(7),
        prepared_reclaim(),
    ));
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
fn full_requires_reclaim_prep_after_remark() {
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
    assert!(!state.active_major_mark_is_ready());
    assert!(!state.active_major_mark_ephemerons_processed());
    assert!(!state.active_major_mark_reclaim_prepared());

    assert!(state.complete_active_major_remark(2, 3));
    assert!(!state.active_major_mark_is_ready());
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

    assert!(state.complete_active_major_reclaim_prep(
        0,
        0,
        Duration::from_nanos(11),
        prepared_reclaim(),
    ));
    assert!(state.active_major_mark_is_ready());
    assert!(state.active_major_mark_reclaim_prepared());
    assert!(state.has_prepared_full_reclaim());
    assert_eq!(
        state
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Reclaim
    );
}

#[test]
fn collector_state_handle_shares_state_across_clones() {
    let handle = CollectorStateHandle::default();
    let clone = handle.clone();

    handle.with_state(|state| state.set_last_completed_plan(Some(major_plan())));

    assert_eq!(clone.lock().last_completed_plan(), Some(major_plan()));
}

#[test]
fn collector_state_handle_try_with_state_reports_would_block_while_locked() {
    let handle = CollectorStateHandle::default();
    let _guard = handle.lock();

    let error = handle
        .try_with_state(|state| state.set_last_completed_plan(Some(major_plan())))
        .expect_err("try_with_state should report contention while the collector is locked");

    assert!(matches!(error, TryLockError::WouldBlock));
}

#[test]
fn collector_state_handle_begin_and_record_reachable_object_updates_progress() {
    let handle = CollectorStateHandle::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let index = [(object.object_key(), 0usize)]
        .into_iter()
        .collect::<ObjectIndex>();
    let objects = [object];

    handle
        .begin_major_mark(&objects, &index, major_plan(), std::iter::empty())
        .expect("begin major mark through handle");
    let recorded = handle
        .record_active_major_reachable_object(&objects, &index, objects[0].erased(), 0)
        .expect("record active major reachable object through handle");

    assert!(recorded);
    assert!(objects[0].is_marked());
    assert_eq!(
        handle
            .major_mark_progress()
            .expect("major mark progress after handle record")
            .remaining_work,
        1
    );
}

#[test]
fn collector_state_handle_begin_major_mark_and_refresh_updates_recommended_plan() {
    let handle = CollectorStateHandle::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let index = [(object.object_key(), 0usize)]
        .into_iter()
        .collect::<ObjectIndex>();
    let objects = [object];

    handle
        .begin_major_mark_and_refresh(
            &objects,
            &index,
            major_plan(),
            [objects[0].erased()],
            &HeapStats::default(),
            &OldGenState::default(),
            &OldGenConfig::default(),
            |kind| CollectionPlan {
                kind,
                ..major_plan()
            },
        )
        .expect("begin and refresh major mark through handle");

    assert_eq!(
        handle
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::ConcurrentMark
    );
    assert_eq!(handle.recommended_plan().kind, CollectionKind::Major);
    assert_eq!(
        handle.recommended_plan().phase,
        CollectionPhase::ConcurrentMark
    );
}

#[test]
fn collector_state_handle_finish_active_collection_if_ready_finishes_prepared_session() {
    let handle = CollectorStateHandle::default();
    handle.with_state(|state| {
        state.begin_major_mark(major_plan(), MarkWorklist::default());
        assert!(state.complete_active_major_reclaim_prep(
            2,
            3,
            Duration::from_nanos(11),
            prepared_reclaim(),
        ));
    });

    let finished = handle
        .finish_active_collection_if_ready(
            &[],
            &ObjectIndex::default(),
            |_tracer, _plan| panic!("prepared session should not re-run remark"),
            |_plan| Ok(prepared_reclaim()),
        )
        .expect("finish prepared active collection through handle")
        .expect("prepared session should finish");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.reclaim_prepare_nanos, 11);
    assert!(!handle.has_active_major_mark());
}

#[test]
fn collector_state_handle_finish_active_collection_now_finishes_prepared_session() {
    let handle = CollectorStateHandle::default();
    handle.with_state(|state| {
        state.begin_major_mark(major_plan(), MarkWorklist::default());
        assert!(state.complete_active_major_reclaim_prep(
            5,
            7,
            Duration::from_nanos(13),
            prepared_reclaim(),
        ));
    });

    let finished = handle
        .finish_active_collection_now(
            &[],
            &ObjectIndex::default(),
            |_tracer, _plan| panic!("prepared session should not re-run remark"),
            |_plan| Ok(prepared_reclaim()),
        )
        .expect("finish prepared active collection immediately through handle");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.mark_steps, 5);
    assert_eq!(finished.mark_rounds, 7);
    assert_eq!(finished.reclaim_prepare_nanos, 13);
    assert!(!handle.has_active_major_mark());
}

#[test]
fn collector_state_handle_prepare_active_collection_reclaim_and_refresh_updates_major_plan() {
    let handle = CollectorStateHandle::default();
    handle.with_state(|state| {
        state.begin_major_mark(major_plan(), MarkWorklist::default());
        assert!(state.complete_active_major_remark(2, 3));
    });
    let request = handle
        .active_reclaim_prep_request()
        .expect("active reclaim prep request");

    let prepared = handle
        .prepare_active_collection_reclaim_with_request_and_refresh(
            request,
            &[],
            &ObjectIndex::default(),
            |_tracer, _plan| (0, 0),
            |_plan| Ok(prepared_reclaim()),
            &HeapStats::default(),
            &OldGenState::default(),
            &OldGenConfig::default(),
            |kind| CollectionPlan {
                kind,
                ..major_plan()
            },
        )
        .expect("prepare and refresh active major reclaim through handle");

    assert!(prepared);
    assert_eq!(
        handle
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Reclaim
    );
    assert_eq!(handle.recommended_plan().kind, CollectionKind::Major);
    assert_eq!(handle.recommended_plan().phase, CollectionPhase::Reclaim);
}

#[test]
fn collector_state_handle_prepare_active_collection_reclaim_and_refresh_updates_full_plan() {
    let handle = CollectorStateHandle::default();
    handle.with_state(|state| {
        state.begin_major_mark(full_plan(), MarkWorklist::default());
        assert!(state.complete_active_major_remark(2, 3));
    });
    let request = handle
        .active_reclaim_prep_request()
        .expect("active reclaim prep request");

    let prepared = handle
        .prepare_active_collection_reclaim_with_request_and_refresh(
            request,
            &[],
            &ObjectIndex::default(),
            |_tracer, _plan| (0, 0),
            |_plan| Ok(prepared_reclaim()),
            &HeapStats::default(),
            &OldGenState::default(),
            &OldGenConfig::default(),
            |kind| CollectionPlan {
                kind,
                ..full_plan()
            },
        )
        .expect("prepare and refresh active full reclaim through handle");

    assert!(prepared);
    assert_eq!(
        handle
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Reclaim
    );
    assert_eq!(handle.recommended_plan().kind, CollectionKind::Full);
    assert_eq!(handle.recommended_plan().phase, CollectionPhase::Reclaim);
}

#[test]
fn collector_state_handle_refresh_cached_plans_prefers_active_session_plan() {
    let handle = CollectorStateHandle::default();
    handle.with_state(|state| state.begin_major_mark(major_plan(), MarkWorklist::default()));

    handle.refresh_cached_plans(
        &HeapStats::default(),
        &OldGenState::default(),
        &OldGenConfig::default(),
        |kind| CollectionPlan {
            kind,
            ..major_plan()
        },
    );

    assert_eq!(handle.recommended_plan().kind, CollectionKind::Major,);
    assert_eq!(
        handle
            .active_major_mark_plan()
            .expect("active major-mark plan")
            .phase,
        CollectionPhase::Remark,
    );
    assert_eq!(handle.recommended_plan().phase, CollectionPhase::Remark);
    assert_eq!(
        handle
            .recommended_background_plan()
            .expect("background plan")
            .phase,
        CollectionPhase::Remark,
    );
}

#[test]
fn collector_state_handle_record_completed_plan_updates_last_plan_and_recommendations() {
    let handle = CollectorStateHandle::default();
    let completed_plan = CollectionPlan {
        phase: CollectionPhase::Reclaim,
        ..major_plan()
    };
    let mut stats = HeapStats::default();
    stats.nursery.live_bytes = 8;

    handle.record_completed_plan(
        completed_plan.clone(),
        &stats,
        &OldGenState::default(),
        &OldGenConfig::default(),
        |kind| CollectionPlan {
            kind,
            ..major_plan()
        },
    );

    assert_eq!(handle.last_completed_plan(), Some(completed_plan));
    assert_eq!(handle.recommended_plan().kind, CollectionKind::Minor);
    assert_eq!(handle.recommended_background_plan(), None);
}
