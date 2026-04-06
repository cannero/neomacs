use crate::collector_exec::MarkTracer;
use crate::collector_state::{CollectorState, MajorMarkUpdate};
use crate::heap::AllocError;
use crate::index_state::ObjectIndex;
use crate::object::ObjectRecord;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress};

pub(crate) fn begin_major_mark(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    plan: CollectionPlan,
    seed_roots: impl FnOnce(&mut MarkTracer<'_>),
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
    seed_roots(&mut tracer);
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
