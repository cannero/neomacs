use super::*;
use crate::mark::MarkWorklist;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::reclaim::{PreparedReclaim, PreparedReclaimSurvivor};
use crate::spaces::OldRegionCollectionStats;

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

fn prepared_reclaim() -> PreparedReclaim {
    PreparedReclaim {
        promoted_bytes: 0,
        rebuilt_old_regions: Vec::new(),
        rebuilt_object_index: std::collections::HashMap::new(),
        old_reserved_bytes: 0,
        old_region_stats: OldRegionCollectionStats::default(),
        survivors: vec![PreparedReclaimSurvivor {
            object_index: 0,
            old_region_placement: None,
        }],
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
fn prepare_active_reclaim_plan_moves_major_session_to_reclaim() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan.clone(), MarkWorklist::default());
    let request = active_reclaim_prep_request(&state).expect("active reclaim prep request");

    let (mark_steps_delta, mark_rounds_delta) =
        prepare_active_reclaim(&request, |_tracer, _plan| (2, 3), &[], &index);
    let prepared =
        build_prepared_active_reclaim(&request, mark_steps_delta, mark_rounds_delta, |_plan| {
            Ok(prepared_reclaim())
        })
        .expect("major reclaim prep should succeed");
    assert!(complete_active_reclaim_prep(&mut state, prepared));

    assert!(state.active_major_mark_is_ready());
    assert!(state.active_major_mark_reclaim_prepared());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Reclaim
    );
    let progress = state.major_mark_progress().expect("major mark progress");
    assert_eq!(progress.mark_steps, 2);
    assert_eq!(progress.mark_rounds, 3);
}

#[test]
fn prepare_active_reclaim_plan_skips_ephemeron_trace_after_remark() {
    let mut state = CollectorState::default();
    let plan = full_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan.clone(), MarkWorklist::default());
    assert!(state.complete_active_major_remark(5, 7));
    let request = active_reclaim_prep_request(&state).expect("active reclaim prep request");

    let (mark_steps_delta, mark_rounds_delta) = prepare_active_reclaim(
        &request,
        |_tracer, _plan| panic!("ephemeron trace should be skipped after remark"),
        &[],
        &index,
    );
    let prepared =
        build_prepared_active_reclaim(&request, mark_steps_delta, mark_rounds_delta, |_plan| {
            Ok(prepared_reclaim())
        })
        .expect("full reclaim prep should succeed");
    assert!(complete_active_reclaim_prep(&mut state, prepared));

    assert!(state.active_major_mark_is_ready());
    assert!(state.has_prepared_full_reclaim());
    let progress = state.major_mark_progress().expect("major mark progress");
    assert_eq!(progress.mark_steps, 5);
    assert_eq!(progress.mark_rounds, 7);
}

#[test]
fn finish_major_mark_updates_state_and_marks_ephemerons_processed() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan, MarkWorklist::default());

    let state = state
        .take_major_mark_state()
        .expect("active major mark state should exist");
    let mut state = state;
    finish_major_mark(&mut state, &[], &index, |_tracer, _plan| (2, 3));

    assert!(state.ephemerons_processed);
    assert!(state.worklist.is_empty());
    assert_eq!(state.mark_steps, 2);
    assert_eq!(state.mark_rounds, 3);
}
