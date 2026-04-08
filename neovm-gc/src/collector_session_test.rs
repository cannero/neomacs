use super::*;
use crate::descriptor::{Relocator, Trace, Tracer, fixed_type_desc};
use crate::index_state::PreparedIndexReclaim;
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::reclaim::{PreparedReclaim, PreparedReclaimSurvivor};
use crate::spaces::{OldRegionCollectionStats, PreparedOldGenReclaim};
use crate::stats::PreparedHeapStats;
use std::collections::HashMap;

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
            region_stats: OldRegionCollectionStats::default(),
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
fn begin_major_mark_seeds_sources_into_initial_worklist() {
    let mut state = CollectorState::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let index = [(object.object_key(), 0usize)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let objects = [object];

    begin_major_mark(
        &mut state,
        &objects,
        &index,
        major_plan(),
        [objects[0].erased()],
    )
    .expect("begin major mark");

    let progress = state.major_mark_progress().expect("major mark progress");
    assert_eq!(progress.remaining_work, 1);
}

#[test]
fn mark_active_major_session_object_marks_and_enqueues_existing_record() {
    let mut state = CollectorState::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let index = [(object.object_key(), 0usize)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let objects = [object];
    state.begin_major_mark(major_plan(), MarkWorklist::default());

    assert!(mark_active_major_session_object(
        &mut state,
        &objects,
        &index,
        objects[0].erased(),
    ));
    assert_eq!(
        state
            .major_mark_progress()
            .expect("major mark progress after enqueue")
            .remaining_work,
        1
    );
    assert!(!mark_active_major_session_object(
        &mut state,
        &objects,
        &index,
        objects[0].erased(),
    ));
}

#[test]
fn assist_active_major_mark_slices_accumulates_progress_across_slices() {
    let mut state = CollectorState::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let first =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate first pinned leaf");
    let second =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate second pinned leaf");
    let index = [(first.object_key(), 0usize), (second.object_key(), 1usize)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let objects = [first, second];
    let mut worklist = MarkWorklist::default();
    worklist.push(0);
    worklist.push(1);
    let mut plan = major_plan();
    plan.mark_slice_budget = 1;
    state.begin_major_mark(plan, worklist);

    let progress = assist_active_major_mark_slices(&mut state, &objects, &index, 2)
        .expect("assist active major mark")
        .expect("active major-mark progress");

    assert!(progress.completed);
    assert_eq!(progress.drained_objects, 2);
    assert_eq!(progress.mark_steps, 2);
    assert_eq!(progress.mark_rounds, 2);
    assert_eq!(progress.remaining_work, 0);
}

#[test]
fn record_active_major_reachable_object_marks_and_enqueues_object() {
    let mut state = CollectorState::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let index = [(object.object_key(), 0usize)]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let objects = [object];
    state.begin_major_mark(major_plan(), MarkWorklist::default());

    let recorded =
        record_active_major_reachable_object(&mut state, &objects, &index, objects[0].erased(), 0)
            .expect("record active major reachable object");

    assert!(recorded);
    assert!(objects[0].is_marked());
    assert_eq!(
        state
            .major_mark_progress()
            .expect("major mark progress after reachable object")
            .remaining_work,
        1
    );
}

#[test]
fn record_active_major_post_write_marks_satb_and_incremental_targets() {
    let mut state = CollectorState::default();
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let owner = ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate owner leaf");
    let old_value =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate old target leaf");
    let new_value =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate new target leaf");
    let index = [
        (owner.object_key(), 0usize),
        (old_value.object_key(), 1usize),
        (new_value.object_key(), 2usize),
    ]
    .into_iter()
    .collect::<HashMap<_, _>>();
    let objects = [owner, old_value, new_value];
    objects[0].mark_if_unmarked();
    state.begin_major_mark(major_plan(), MarkWorklist::default());

    let recorded = record_active_major_post_write(
        &mut state,
        &objects,
        &index,
        objects[0].erased(),
        Some(objects[1].erased()),
        Some(objects[2].erased()),
        0,
    )
    .expect("record active major post write");

    assert!(recorded);
    assert!(objects[1].is_marked());
    assert!(objects[2].is_marked());
    assert_eq!(
        state
            .major_mark_progress()
            .expect("major mark progress after post write")
            .remaining_work,
        2
    );
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
fn prepare_active_reclaim_request_moves_major_session_to_reclaim() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan.clone(), MarkWorklist::default());
    let request = active_reclaim_prep_request(&state).expect("active reclaim prep request");

    let prepared = prepare_active_reclaim_request(
        request,
        |_tracer, _plan| (2, 3),
        &[],
        &index,
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("major reclaim prep should succeed");
    let completed = complete_active_reclaim_prep(&mut state, prepared);

    assert!(completed);
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
fn prepare_active_reclaim_request_moves_full_session_to_reclaim() {
    let mut state = CollectorState::default();
    state.begin_major_mark(full_plan(), MarkWorklist::default());
    assert!(state.complete_active_major_remark(5, 7));
    let request = active_reclaim_prep_request(&state).expect("active reclaim prep request");

    let prepared = prepare_active_reclaim_request(
        request,
        |_tracer, _plan| (0, 0),
        &[],
        &ObjectIndex::default(),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("full reclaim prep should succeed");
    let completed = complete_active_reclaim_prep(&mut state, prepared);

    assert!(completed);
    assert!(state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Reclaim
    );
}

#[test]
fn prepare_active_collection_reclaim_if_needed_moves_full_session_to_reclaim() {
    let mut state = CollectorState::default();
    let plan = full_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan, MarkWorklist::default());
    assert!(state.complete_active_major_remark(5, 7));

    let completed = prepare_active_collection_reclaim_if_needed(
        &mut state,
        &[],
        &index,
        |_tracer, _plan| panic!("remarked full session should skip ephemeron trace"),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("full reclaim prep should succeed");

    assert!(completed);
    assert!(state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Reclaim
    );
}

#[test]
fn prepare_active_collection_reclaim_if_needed_returns_false_without_request() {
    let mut state = CollectorState::default();

    let completed = prepare_active_collection_reclaim_if_needed(
        &mut state,
        &[],
        &ObjectIndex::default(),
        |_tracer, _plan| panic!("inactive session should not trace"),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("inactive session should be ignored");

    assert!(!completed);
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
fn take_or_prepare_reclaim_for_finish_returns_existing_prepared_reclaim() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    state.begin_major_mark(plan, MarkWorklist::default());
    assert!(state.complete_active_major_reclaim_prep(
        2,
        3,
        Duration::from_nanos(11),
        prepared_reclaim()
    ));

    let mut state = state
        .take_major_mark_state()
        .expect("active major mark state should exist");
    let (prepared, reclaim_prepare_nanos) =
        take_or_prepare_reclaim_for_finish(&mut state, |_plan| {
            panic!("existing prepared reclaim should be reused")
        })
        .expect("take prepared reclaim");

    assert_eq!(reclaim_prepare_nanos, 11);
    assert_eq!(prepared.survivors.len(), 1);
}

#[test]
fn take_or_prepare_reclaim_for_finish_builds_missing_reclaim() {
    let mut state = CollectorState::default();
    let plan = full_plan();
    state.begin_major_mark(plan, MarkWorklist::default());
    assert!(state.complete_active_major_remark(5, 7));

    let mut state = state
        .take_major_mark_state()
        .expect("active major mark state should exist");
    let (prepared, reclaim_prepare_nanos) =
        take_or_prepare_reclaim_for_finish(&mut state, |_plan| Ok(prepared_reclaim()))
            .expect("build missing prepared reclaim");

    assert_eq!(prepared.survivors.len(), 1);
    assert!(reclaim_prepare_nanos > 0);
}

#[test]
fn complete_drained_major_mark_round_moves_major_session_to_reclaim() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan.clone(), MarkWorklist::default());

    let completed = complete_drained_major_mark_round(
        &mut state,
        &[],
        &index,
        |_tracer, _plan| (2, 3),
        |_plan| prepared_reclaim(),
    );

    assert!(completed);
    assert!(state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Reclaim
    );
    let progress = state.major_mark_progress().expect("major mark progress");
    assert_eq!(progress.mark_steps, 2);
    assert_eq!(progress.mark_rounds, 3);
}

#[test]
fn complete_drained_major_mark_round_moves_full_session_to_remark() {
    let mut state = CollectorState::default();
    let plan = full_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan.clone(), MarkWorklist::default());

    let completed = complete_drained_major_mark_round(
        &mut state,
        &[],
        &index,
        |_tracer, _plan| (5, 7),
        |_plan| panic!("full round should not prepare reclaim yet"),
    );

    assert!(completed);
    assert!(!state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Remark
    );
    let progress = state.major_mark_progress().expect("major mark progress");
    assert_eq!(progress.mark_steps, 5);
    assert_eq!(progress.mark_rounds, 7);
}

#[test]
fn poll_active_major_mark_with_completion_moves_major_session_to_reclaim() {
    let mut state = CollectorState::default();
    let plan = major_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan, MarkWorklist::default());

    let progress = poll_active_major_mark_with_completion(
        &mut state,
        &[],
        &index,
        |_tracer, _plan| (2, 3),
        |_plan| prepared_reclaim(),
    )
    .expect("poll active major mark");

    let progress = progress.expect("major mark progress");
    assert!(progress.completed);
    assert!(state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Reclaim
    );
}

#[test]
fn poll_active_major_mark_with_completion_moves_full_session_to_remark() {
    let mut state = CollectorState::default();
    let plan = full_plan();
    let index = ObjectIndex::default();
    state.begin_major_mark(plan, MarkWorklist::default());

    let progress = poll_active_major_mark_with_completion(
        &mut state,
        &[],
        &index,
        |_tracer, _plan| (5, 7),
        |_plan| panic!("full poll should not build reclaim yet"),
    )
    .expect("poll active full mark");

    let progress = progress.expect("major mark progress");
    assert!(progress.completed);
    assert!(!state.active_major_mark_is_ready());
    assert_eq!(
        state.active_major_mark_plan().expect("active plan").phase,
        CollectionPhase::Remark
    );
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

#[test]
fn finish_active_collection_reuses_existing_prepared_reclaim() {
    let mut collector = CollectorState::default();
    collector.begin_major_mark(major_plan(), MarkWorklist::default());
    assert!(collector.complete_active_major_reclaim_prep(
        2,
        3,
        Duration::from_nanos(11),
        prepared_reclaim()
    ));
    let state = collector
        .take_major_mark_state()
        .expect("active major mark state should exist");

    let finished = finish_active_collection(state, |_plan| {
        panic!("existing prepared reclaim should be reused")
    })
    .expect("finish active collection");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.completed_plan.kind, CollectionKind::Major);
    assert_eq!(finished.mark_steps, 2);
    assert_eq!(finished.mark_rounds, 3);
    assert_eq!(finished.reclaim_prepare_nanos, 11);
    assert_eq!(finished.prepared_reclaim.survivors.len(), 1);
}

#[test]
fn finish_active_collection_builds_missing_full_reclaim() {
    let mut collector = CollectorState::default();
    collector.begin_major_mark(full_plan(), MarkWorklist::default());
    assert!(collector.complete_active_major_remark(5, 7));
    let state = collector
        .take_major_mark_state()
        .expect("active major mark state should exist");

    let finished = finish_active_collection(state, |_plan| Ok(prepared_reclaim()))
        .expect("finish active collection");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.completed_plan.kind, CollectionKind::Full);
    assert_eq!(finished.mark_steps, 5);
    assert_eq!(finished.mark_rounds, 7);
    assert!(finished.reclaim_prepare_nanos > 0);
    assert_eq!(finished.prepared_reclaim.survivors.len(), 1);
}

#[test]
fn finalize_active_collection_state_finishes_major_reclaim_state() {
    let mut collector = CollectorState::default();
    collector.begin_major_mark(major_plan(), MarkWorklist::default());
    let state = collector
        .take_major_mark_state()
        .expect("active major mark state should exist");

    let finished = finalize_active_collection_state(
        state,
        &[],
        &ObjectIndex::default(),
        |_tracer, _plan| (2, 3),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("finalize active collection state");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.mark_steps, 2);
    assert_eq!(finished.mark_rounds, 3);
}

#[test]
fn finish_active_collection_if_ready_returns_none_when_not_ready() {
    let mut collector = CollectorState::default();
    collector.begin_major_mark(major_plan(), MarkWorklist::default());

    let finished = finish_active_collection_if_ready(
        &mut collector,
        &[],
        &ObjectIndex::default(),
        |_tracer, _plan| panic!("unfinished session should not trace"),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("finish active collection if ready");

    assert!(finished.is_none());
    assert!(collector.has_active_major_mark());
}

#[test]
fn finish_active_collection_if_ready_takes_prepared_major_session() {
    let mut collector = CollectorState::default();
    collector.begin_major_mark(major_plan(), MarkWorklist::default());
    assert!(collector.complete_active_major_reclaim_prep(
        2,
        3,
        Duration::from_nanos(11),
        prepared_reclaim()
    ));

    let finished = finish_active_collection_if_ready(
        &mut collector,
        &[],
        &ObjectIndex::default(),
        |_tracer, _plan| panic!("prepared session should not re-run remark"),
        |_plan| Ok(prepared_reclaim()),
    )
    .expect("finish active collection if ready")
    .expect("prepared session should finish");

    assert_eq!(finished.completed_plan.phase, CollectionPhase::Reclaim);
    assert_eq!(finished.reclaim_prepare_nanos, 11);
    assert!(!collector.has_active_major_mark());
}
