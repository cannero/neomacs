use crate::descriptor::{GcErased, Tracer};
use std::time::{Duration, Instant};

use crate::collector_exec::MarkTracer;
use crate::collector_state::{CollectorState, MajorMarkState, MajorMarkUpdate};
use crate::heap::AllocError;
use crate::index_state::ObjectIndex;
use crate::object::ObjectRecord;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress};
use crate::reclaim::PreparedReclaim;

#[derive(Clone, Debug)]
pub(crate) struct ActiveReclaimPrepRequest {
    pub(crate) plan: CollectionPlan,
    pub(crate) ephemerons_processed: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedActiveReclaim {
    pub(crate) mark_steps_delta: u64,
    pub(crate) mark_rounds_delta: u64,
    pub(crate) reclaim_prepare_time: Duration,
    pub(crate) prepared_reclaim: PreparedReclaim,
}

#[derive(Debug)]
pub(crate) struct FinishedActiveCollection {
    pub(crate) completed_plan: CollectionPlan,
    pub(crate) mark_steps: u64,
    pub(crate) mark_rounds: u64,
    pub(crate) reclaim_prepare_nanos: u64,
    pub(crate) prepared_reclaim: PreparedReclaim,
}

pub(crate) fn begin_major_mark(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    plan: CollectionPlan,
    sources: impl IntoIterator<Item = crate::descriptor::GcErased>,
) -> Result<(), AllocError> {
    if collector.has_active_major_mark() {
        return Err(AllocError::CollectionInProgress);
    }
    if !matches!(plan.kind, CollectionKind::Major | CollectionKind::Full) {
        return Err(AllocError::UnsupportedCollectionKind { kind: plan.kind });
    }

    collector.clear_recent_phase_trace();
    for object in objects {
        object.clear_mark();
    }

    collector.push_phase(CollectionPhase::InitialMark);
    if plan.concurrent {
        collector.push_phase(CollectionPhase::ConcurrentMark);
    }

    let mut tracer = MarkTracer::new(objects, index);
    for source in sources {
        tracer.mark_erased(source);
    }
    collector.begin_major_mark(plan, tracer.into_worklist());
    Ok(())
}

pub(crate) fn advance_major_mark_slice(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> Result<MajorMarkProgress, AllocError> {
    collector.update_active_major_mark(|plan, worklist| {
        let mut tracer = MarkTracer::with_worklist(objects, index, worklist);
        let drained_objects = tracer.drain_one_slice(plan.mark_slice_budget);
        MajorMarkUpdate {
            worklist: tracer.into_worklist(),
            drained_objects,
            mark_steps_delta: u64::from(drained_objects > 0),
            mark_rounds_delta: u64::from(drained_objects > 0),
        }
    })
}

pub(crate) fn assist_active_major_mark_slices(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    max_slices: usize,
) -> Result<Option<MajorMarkProgress>, AllocError> {
    if !collector.has_active_major_mark() {
        return Ok(None);
    }
    if max_slices == 0 {
        return Ok(collector.major_mark_progress());
    }

    let mut total_drained_objects = 0usize;
    let mut final_progress = None;
    for _ in 0..max_slices {
        let progress = advance_major_mark_slice(collector, objects, index)?;
        total_drained_objects = total_drained_objects.saturating_add(progress.drained_objects);
        let completed = progress.completed;
        final_progress = Some(progress);
        if completed {
            break;
        }
    }

    Ok(final_progress.map(|progress| MajorMarkProgress {
        completed: progress.completed,
        drained_objects: total_drained_objects,
        elapsed_nanos: progress.elapsed_nanos,
        mark_steps: progress.mark_steps,
        mark_rounds: progress.mark_rounds,
        remaining_work: progress.remaining_work,
    }))
}

pub(crate) fn mark_active_major_session_object(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    object: GcErased,
) -> bool {
    if !collector.has_active_major_mark() {
        return false;
    }

    let Some(&object_index) = index.get(&object.object_key()) else {
        return false;
    };

    let record = &objects[object_index];
    if !record.mark_if_unmarked() {
        return false;
    }

    collector.enqueue_active_major_mark_index(object_index)
}

pub(crate) fn record_active_major_reachable_object(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    object: GcErased,
    assist_slices: usize,
) -> Result<bool, AllocError> {
    if !collector.has_active_major_mark() {
        return Ok(false);
    }

    let _enqueued = mark_active_major_session_object(collector, objects, index, object);
    if assist_slices > 0 {
        let _progress = assist_active_major_mark_slices(collector, objects, index, assist_slices)?;
    }
    Ok(true)
}

fn is_marked_active_major_session_object(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    object: GcErased,
) -> bool {
    let Some(&object_index) = index.get(&object.object_key()) else {
        return false;
    };
    let record = &objects[object_index];
    record.space() == crate::object::SpaceKind::Immortal || record.is_marked()
}

pub(crate) fn record_active_major_post_write(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    owner: GcErased,
    old_value: Option<GcErased>,
    new_value: Option<GcErased>,
    assist_slices: usize,
) -> Result<bool, AllocError> {
    if !collector.has_active_major_mark() {
        return Ok(false);
    }

    if let Some(value) = old_value {
        let _enqueued = mark_active_major_session_object(collector, objects, index, value);
    }
    if is_marked_active_major_session_object(objects, index, owner)
        && let Some(value) = new_value
    {
        let _enqueued = mark_active_major_session_object(collector, objects, index, value);
    }
    if assist_slices > 0 {
        let _progress = assist_active_major_mark_slices(collector, objects, index, assist_slices)?;
    }
    Ok(true)
}

pub(crate) fn poll_active_major_mark_round(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> Result<Option<MajorMarkProgress>, AllocError> {
    if !collector.has_active_major_mark() {
        return Ok(None);
    }
    collector
        .update_active_major_mark(|plan, worklist| {
            let mut tracer = MarkTracer::with_worklist(objects, index, worklist);
            let (drained_objects, drained_slices) =
                tracer.drain_worker_round(plan.worker_count.max(1), plan.mark_slice_budget);
            MajorMarkUpdate {
                worklist: tracer.into_worklist(),
                drained_objects,
                mark_steps_delta: drained_slices,
                mark_rounds_delta: u64::from(drained_objects > 0),
            }
        })
        .map(Some)
}

pub(crate) fn poll_active_major_mark_with_completion(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_major_reclaim: impl FnOnce(&CollectionPlan) -> PreparedReclaim,
) -> Result<Option<MajorMarkProgress>, AllocError> {
    let progress = poll_active_major_mark_round(collector, objects, index)?;
    if let Some(progress) = progress.as_ref()
        && progress.completed
    {
        complete_drained_major_mark_round(
            collector,
            objects,
            index,
            trace_ephemerons,
            prepare_major_reclaim,
        );
    }
    Ok(progress)
}

pub(crate) fn active_reclaim_prep_request(
    collector: &CollectorState,
) -> Option<ActiveReclaimPrepRequest> {
    collector
        .active_major_mark_needs_reclaim_prep_plan()
        .map(|plan| ActiveReclaimPrepRequest {
            plan,
            ephemerons_processed: collector.active_major_mark_ephemerons_processed(),
        })
}

pub(crate) fn prepare_active_reclaim(
    request: &ActiveReclaimPrepRequest,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> (u64, u64) {
    let mut mark_steps_delta = 0u64;
    let mut mark_rounds_delta = 0u64;
    if !request.ephemerons_processed {
        let mut tracer = MarkTracer::with_worklist(objects, index, Default::default());
        let (ephemeron_steps, ephemeron_rounds) = trace_ephemerons(&mut tracer, &request.plan);
        mark_steps_delta = mark_steps_delta.saturating_add(ephemeron_steps);
        mark_rounds_delta = mark_rounds_delta.saturating_add(ephemeron_rounds);
    }
    (mark_steps_delta, mark_rounds_delta)
}

pub(crate) fn build_prepared_active_reclaim(
    request: &ActiveReclaimPrepRequest,
    mark_steps_delta: u64,
    mark_rounds_delta: u64,
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<PreparedActiveReclaim, AllocError> {
    let reclaim_prepare_start = Instant::now();
    let prepared_reclaim = prepare_reclaim(&request.plan)?;
    Ok(PreparedActiveReclaim {
        mark_steps_delta,
        mark_rounds_delta,
        reclaim_prepare_time: reclaim_prepare_start.elapsed(),
        prepared_reclaim,
    })
}

pub(crate) fn prepare_active_reclaim_request(
    request: ActiveReclaimPrepRequest,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<PreparedActiveReclaim, AllocError> {
    let (mark_steps_delta, mark_rounds_delta) =
        prepare_active_reclaim(&request, trace_ephemerons, objects, index);
    build_prepared_active_reclaim(
        &request,
        mark_steps_delta,
        mark_rounds_delta,
        prepare_reclaim,
    )
}

#[cfg(test)]
pub(crate) fn prepare_active_collection_reclaim_if_needed(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<bool, AllocError> {
    let Some(request) = active_reclaim_prep_request(collector) else {
        return Ok(false);
    };
    let prepared =
        prepare_active_reclaim_request(request, trace_ephemerons, objects, index, prepare_reclaim)?;
    Ok(complete_active_reclaim_prep(collector, prepared))
}

pub(crate) fn complete_active_reclaim_prep(
    collector: &mut CollectorState,
    prepared: PreparedActiveReclaim,
) -> bool {
    let completed = collector.complete_active_major_reclaim_prep(
        prepared.mark_steps_delta,
        prepared.mark_rounds_delta,
        prepared.reclaim_prepare_time,
        prepared.prepared_reclaim,
    );
    debug_assert!(
        completed,
        "active major reclaim prep should only complete while the session stays active"
    );
    completed
}

pub(crate) fn take_or_prepare_reclaim_for_finish(
    state: &mut MajorMarkState,
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<(PreparedReclaim, u64), AllocError> {
    let mut reclaim_prepare_nanos = state.reclaim_prepare_nanos;
    let prepared_reclaim = if state.reclaim_prepared {
        state.prepared_reclaim.take()
    } else {
        let request = ActiveReclaimPrepRequest {
            plan: state.plan.clone(),
            ephemerons_processed: state.ephemerons_processed,
        };
        let prepared = build_prepared_active_reclaim(&request, 0, 0, prepare_reclaim)?;
        if reclaim_prepare_nanos == 0 {
            reclaim_prepare_nanos = saturating_duration_nanos(prepared.reclaim_prepare_time);
        }
        Some(prepared.prepared_reclaim)
    };
    let prepared_reclaim =
        prepared_reclaim.expect("major/full finish should always have prepared reclaim");
    Ok((prepared_reclaim, reclaim_prepare_nanos))
}

pub(crate) fn finish_active_collection(
    mut state: MajorMarkState,
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<FinishedActiveCollection, AllocError> {
    let (prepared_reclaim, reclaim_prepare_nanos) =
        take_or_prepare_reclaim_for_finish(&mut state, prepare_reclaim)?;

    Ok(FinishedActiveCollection {
        completed_plan: CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..state.plan
        },
        mark_steps: state.mark_steps,
        mark_rounds: state.mark_rounds,
        reclaim_prepare_nanos,
        prepared_reclaim,
    })
}

pub(crate) fn finish_active_collection_now(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<FinishedActiveCollection, AllocError> {
    let Some(state) = collector.take_major_mark_state() else {
        return Err(AllocError::NoCollectionInProgress);
    };
    finalize_active_collection_state(state, objects, index, trace_ephemerons, prepare_reclaim)
}

pub(crate) fn finalize_active_collection_state(
    mut state: MajorMarkState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<FinishedActiveCollection, AllocError> {
    finish_major_mark(&mut state, objects, index, trace_ephemerons);
    finish_active_collection(state, prepare_reclaim)
}

pub(crate) fn finish_active_collection_if_ready(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<Option<FinishedActiveCollection>, AllocError> {
    if !collector.active_major_mark_is_ready() {
        return Ok(None);
    }
    finish_active_collection_now(collector, objects, index, trace_ephemerons, prepare_reclaim)
        .map(Some)
}

pub(crate) fn complete_drained_major_mark_round(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    prepare_major_reclaim: impl FnOnce(&CollectionPlan) -> PreparedReclaim,
) -> bool {
    let Some(plan) = collector.active_major_mark_needs_reclaim_prep_plan() else {
        return false;
    };
    if collector.active_major_mark_ephemerons_processed()
        || !matches!(plan.kind, CollectionKind::Major | CollectionKind::Full)
    {
        return false;
    }

    let mut tracer = MarkTracer::with_worklist(objects, index, Default::default());
    let (ephemeron_steps, ephemeron_rounds) = trace_ephemerons(&mut tracer, &plan);
    if plan.kind == CollectionKind::Major {
        let reclaim_prepare_start = Instant::now();
        let prepared_reclaim = prepare_major_reclaim(&plan);
        collector.complete_active_major_reclaim_prep(
            ephemeron_steps,
            ephemeron_rounds,
            reclaim_prepare_start.elapsed(),
            prepared_reclaim,
        )
    } else {
        collector.complete_active_major_remark(ephemeron_steps, ephemeron_rounds)
    }
}

pub(crate) fn finish_major_mark(
    state: &mut MajorMarkState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
) {
    if state.ephemerons_processed {
        return;
    }

    let mut tracer =
        MarkTracer::with_worklist(objects, index, core::mem::take(&mut state.worklist));
    let (mark_steps, mark_rounds) = tracer
        .drain_parallel_until_empty(state.plan.worker_count.max(1), state.plan.mark_slice_budget);
    state.mark_steps = state.mark_steps.saturating_add(mark_steps);
    state.mark_rounds = state.mark_rounds.saturating_add(mark_rounds);
    let (ephemeron_steps, ephemeron_rounds) = trace_ephemerons(&mut tracer, &state.plan);
    state.mark_steps = state.mark_steps.saturating_add(ephemeron_steps);
    state.mark_rounds = state.mark_rounds.saturating_add(ephemeron_rounds);
    state.worklist = tracer.into_worklist();
    state.mark_elapsed_nanos = saturating_duration_nanos(state.mark_started_at.elapsed());
    state.ephemerons_processed = true;
}

fn saturating_duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
#[path = "collector_session_test.rs"]
mod tests;
