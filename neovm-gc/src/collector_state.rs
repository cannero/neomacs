use std::sync::{Arc, Mutex, MutexGuard, TryLockError, TryLockResult};
use std::time::{Duration, Instant};

use crate::collector_exec::MarkTracer;
use crate::collector_policy::refresh_cached_plans as refresh_cached_collector_plans;
use crate::collector_session::{
    self, ActiveReclaimPrepRequest, FinishedActiveCollection, PreparedActiveReclaim,
};
use crate::heap::AllocError;
use crate::index_state::ObjectIndex;
use crate::mark::MarkWorklist;
use crate::object::ObjectRecord;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress};
use crate::reclaim::PreparedReclaim;
use crate::spaces::{OldGenConfig, OldGenState};
use crate::stats::HeapStats;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CollectorSharedSnapshot {
    pub(crate) recommended_plan: CollectionPlan,
    pub(crate) recommended_background_plan: Option<CollectionPlan>,
    pub(crate) last_completed_plan: Option<CollectionPlan>,
    pub(crate) active_major_mark_plan: Option<CollectionPlan>,
    pub(crate) major_mark_progress: Option<MajorMarkProgress>,
}

#[derive(Debug, Default)]
pub(crate) struct CollectorState {
    recent_phase_trace: Vec<CollectionPhase>,
    last_completed_plan: Option<CollectionPlan>,
    major_mark_state: Option<MajorMarkState>,
    cached_recommended_plan: CollectionPlan,
    cached_recommended_background_plan: Option<CollectionPlan>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CollectorStateHandle {
    state: Arc<Mutex<CollectorState>>,
}

impl CollectorStateHandle {
    pub(crate) fn lock(&self) -> MutexGuard<'_, CollectorState> {
        self.state
            .lock()
            .expect("collector state should not be poisoned")
    }

    pub(crate) fn try_lock(&self) -> TryLockResult<MutexGuard<'_, CollectorState>> {
        self.state.try_lock()
    }

    pub(crate) fn with_state<R>(&self, f: impl FnOnce(&mut CollectorState) -> R) -> R {
        let mut state = self.lock();
        f(&mut state)
    }

    pub(crate) fn try_with_state<R>(
        &self,
        f: impl FnOnce(&mut CollectorState) -> R,
    ) -> Result<R, TryLockError<MutexGuard<'_, CollectorState>>> {
        let mut state = self.try_lock()?;
        Ok(f(&mut state))
    }

    pub(crate) fn shared_snapshot(&self) -> CollectorSharedSnapshot {
        self.lock().shared_snapshot()
    }

    pub(crate) fn recent_phase_trace(&self) -> Vec<CollectionPhase> {
        self.lock().recent_phase_trace().to_vec()
    }

    pub(crate) fn clear_recent_phase_trace(&self) {
        self.with_state(|state| state.clear_recent_phase_trace());
    }

    pub(crate) fn push_phase(&self, phase: CollectionPhase) {
        self.with_state(|state| state.push_phase(phase));
    }

    pub(crate) fn push_phases(&self, phases: impl IntoIterator<Item = CollectionPhase>) {
        self.with_state(|state| {
            for phase in phases {
                state.push_phase(phase);
            }
        });
    }

    pub(crate) fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.lock().last_completed_plan()
    }

    pub(crate) fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.lock().active_major_mark_plan()
    }

    pub(crate) fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.lock().major_mark_progress()
    }

    pub(crate) fn has_active_major_mark(&self) -> bool {
        self.lock().has_active_major_mark()
    }

    pub(crate) fn has_prepared_full_reclaim(&self) -> bool {
        self.lock().has_prepared_full_reclaim()
    }

    pub(crate) fn take_major_mark_state(&self) -> Option<MajorMarkState> {
        self.with_state(CollectorState::take_major_mark_state)
    }

    pub(crate) fn recommended_plan(&self) -> CollectionPlan {
        self.lock().recommended_plan()
    }

    pub(crate) fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.lock().recommended_background_plan()
    }

    pub(crate) fn refresh_cached_plans(
        &self,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) {
        self.with_state(|state| {
            refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for)
        });
    }

    pub(crate) fn record_completed_plan(
        &self,
        completed_plan: CollectionPlan,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) {
        self.with_state(|state| {
            state.set_last_completed_plan(Some(completed_plan));
            refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for);
        });
    }

    pub(crate) fn begin_major_mark_and_refresh(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        plan: CollectionPlan,
        sources: impl IntoIterator<Item = crate::descriptor::GcErased>,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<(), AllocError> {
        self.with_state(|state| {
            collector_session::begin_major_mark(state, objects, index, plan, sources)?;
            refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for);
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_major_mark(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        plan: CollectionPlan,
        sources: impl IntoIterator<Item = crate::descriptor::GcErased>,
    ) -> Result<(), AllocError> {
        self.with_state(|state| {
            collector_session::begin_major_mark(state, objects, index, plan, sources)
        })
    }

    pub(crate) fn assist_active_major_mark_slices_and_refresh(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        max_slices: usize,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.with_state(|state| {
            let progress = collector_session::assist_active_major_mark_slices(
                state, objects, index, max_slices,
            )?;
            refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for);
            Ok(progress)
        })
    }

    #[cfg(test)]
    pub(crate) fn record_active_major_reachable_object(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        object: crate::descriptor::GcErased,
        assist_slices: usize,
    ) -> Result<bool, AllocError> {
        self.with_state(|state| {
            collector_session::record_active_major_reachable_object(
                state,
                objects,
                index,
                object,
                assist_slices,
            )
        })
    }

    /// Hot-path variant: `stats_fn` is only called when the
    /// post-write assist actually updated collector state and
    /// therefore needs to refresh the cached plans. The common
    /// case (no active major-mark session) takes the early
    /// return inside `record_active_major_reachable_object`
    /// and the closure is never invoked, so the caller avoids
    /// computing a full `HeapStats` on every allocation.
    pub(crate) fn record_active_major_reachable_object_and_refresh(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        object: crate::descriptor::GcErased,
        assist_slices: usize,
        stats_fn: impl FnOnce() -> HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<bool, AllocError> {
        self.with_state(|state| {
            let recorded = collector_session::record_active_major_reachable_object(
                state,
                objects,
                index,
                object,
                assist_slices,
            )?;
            if recorded {
                let stats = stats_fn();
                refresh_cached_collector_plans(state, &stats, old_gen, old_config, plan_for);
            }
            Ok(recorded)
        })
    }

    /// Hot-path variant: same lazy-`stats` treatment as
    /// `record_active_major_reachable_object_and_refresh`. In
    /// the common case (no active major-mark session) the
    /// closure is never invoked, so the barrier hot path
    /// avoids computing a full `HeapStats` on every call.
    pub(crate) fn record_active_major_post_write_and_refresh(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        owner: crate::descriptor::GcErased,
        old_value: Option<crate::descriptor::GcErased>,
        new_value: Option<crate::descriptor::GcErased>,
        assist_slices: usize,
        stats_fn: impl FnOnce() -> HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<bool, AllocError> {
        self.with_state(|state| {
            let updated = collector_session::record_active_major_post_write(
                state,
                objects,
                index,
                owner,
                old_value,
                new_value,
                assist_slices,
            )?;
            if updated {
                let stats = stats_fn();
                refresh_cached_collector_plans(state, &stats, old_gen, old_config, plan_for);
            }
            Ok(updated)
        })
    }

    pub(crate) fn poll_active_major_mark_with_completion_and_refresh(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
        prepare_major_reclaim: impl FnOnce(&CollectionPlan) -> PreparedReclaim,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.with_state(|state| {
            let progress = collector_session::poll_active_major_mark_with_completion(
                state,
                objects,
                index,
                trace_ephemerons,
                prepare_major_reclaim,
            )?;
            refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for);
            Ok(progress)
        })
    }

    pub(crate) fn prepare_active_collection_reclaim_with_request_and_refresh(
        &self,
        request: ActiveReclaimPrepRequest,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
        prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> Result<bool, AllocError> {
        let prepared = collector_session::prepare_active_reclaim_request(
            request,
            trace_ephemerons,
            objects,
            index,
            prepare_reclaim,
        )?;
        Ok(self.complete_active_reclaim_prep_and_refresh(
            prepared, stats, old_gen, old_config, plan_for,
        ))
    }

    pub(crate) fn active_reclaim_prep_request(&self) -> Option<ActiveReclaimPrepRequest> {
        let state = self.lock();
        collector_session::active_reclaim_prep_request(&state)
    }

    pub(crate) fn complete_active_reclaim_prep_and_refresh(
        &self,
        prepared: PreparedActiveReclaim,
        stats: &HeapStats,
        old_gen: &OldGenState,
        old_config: &OldGenConfig,
        plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
    ) -> bool {
        self.with_state(|state| {
            let completed = collector_session::complete_active_reclaim_prep(state, prepared);
            if completed {
                refresh_cached_collector_plans(state, stats, old_gen, old_config, plan_for);
            }
            completed
        })
    }

    pub(crate) fn finish_active_collection_if_ready(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
        prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
    ) -> Result<Option<FinishedActiveCollection>, AllocError> {
        self.with_state(|state| {
            collector_session::finish_active_collection_if_ready(
                state,
                objects,
                index,
                trace_ephemerons,
                prepare_reclaim,
            )
        })
    }

    pub(crate) fn finish_active_collection_now(
        &self,
        objects: &[ObjectRecord],
        index: &ObjectIndex,
        trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
        prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
    ) -> Result<FinishedActiveCollection, AllocError> {
        self.with_state(|state| {
            collector_session::finish_active_collection_now(
                state,
                objects,
                index,
                trace_ephemerons,
                prepare_reclaim,
            )
        })
    }
}

#[derive(Debug)]
pub(crate) struct MajorMarkState {
    pub(crate) plan: CollectionPlan,
    pub(crate) worklist: MarkWorklist<usize>,
    pub(crate) mark_started_at: Instant,
    pub(crate) mark_elapsed_nanos: u64,
    pub(crate) mark_steps: u64,
    pub(crate) mark_rounds: u64,
    pub(crate) reclaim_prepare_nanos: u64,
    pub(crate) ephemerons_processed: bool,
    pub(crate) reclaim_prepared: bool,
    pub(crate) prepared_reclaim: Option<PreparedReclaim>,
}

pub(crate) struct MajorMarkUpdate {
    pub(crate) worklist: MarkWorklist<usize>,
    pub(crate) drained_objects: usize,
    pub(crate) mark_steps_delta: u64,
    pub(crate) mark_rounds_delta: u64,
}

impl CollectorState {
    pub(crate) fn recent_phase_trace(&self) -> &[CollectionPhase] {
        &self.recent_phase_trace
    }

    pub(crate) fn clear_recent_phase_trace(&mut self) {
        self.recent_phase_trace.clear();
    }

    pub(crate) fn push_phase(&mut self, phase: CollectionPhase) {
        self.recent_phase_trace.push(phase);
    }

    pub(crate) fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.last_completed_plan.clone()
    }

    pub(crate) fn set_last_completed_plan(&mut self, plan: Option<CollectionPlan>) {
        self.last_completed_plan = plan;
    }

    pub(crate) fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.major_mark_state.as_ref().map(|state| CollectionPlan {
            phase: if state.worklist.is_empty() {
                if state.reclaim_prepared {
                    CollectionPhase::Reclaim
                } else {
                    CollectionPhase::Remark
                }
            } else {
                CollectionPhase::ConcurrentMark
            },
            ..state.plan.clone()
        })
    }

    pub(crate) fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.major_mark_state
            .as_ref()
            .map(|state| MajorMarkProgress {
                completed: state.worklist.is_empty(),
                drained_objects: 0,
                elapsed_nanos: state.mark_elapsed_nanos,
                mark_steps: state.mark_steps,
                mark_rounds: state.mark_rounds,
                remaining_work: state.worklist.len(),
            })
    }

    pub(crate) fn has_active_major_mark(&self) -> bool {
        self.major_mark_state.is_some()
    }

    pub(crate) fn begin_major_mark(&mut self, plan: CollectionPlan, worklist: MarkWorklist<usize>) {
        self.major_mark_state = Some(MajorMarkState {
            plan,
            worklist,
            mark_started_at: Instant::now(),
            mark_elapsed_nanos: 0,
            mark_steps: 0,
            mark_rounds: 0,
            reclaim_prepare_nanos: 0,
            ephemerons_processed: false,
            reclaim_prepared: false,
            prepared_reclaim: None,
        });
    }

    pub(crate) fn enqueue_active_major_mark_index(&mut self, index: usize) -> bool {
        let Some(state) = self.major_mark_state.as_mut() else {
            return false;
        };
        state.worklist.push(index);
        state.mark_elapsed_nanos = saturating_duration_nanos(state.mark_started_at.elapsed());
        state.reclaim_prepare_nanos = 0;
        state.ephemerons_processed = false;
        state.reclaim_prepared = false;
        state.prepared_reclaim = None;
        true
    }

    pub(crate) fn take_major_mark_state(&mut self) -> Option<MajorMarkState> {
        self.major_mark_state.take()
    }

    pub(crate) fn active_major_mark_is_ready(&self) -> bool {
        self.major_mark_state
            .as_ref()
            .is_some_and(|state| state.worklist.is_empty() && state.reclaim_prepared)
    }

    pub(crate) fn active_major_mark_needs_reclaim_prep_plan(&self) -> Option<CollectionPlan> {
        self.major_mark_state
            .as_ref()
            .filter(|state| state.worklist.is_empty() && !state.reclaim_prepared)
            .map(|state| state.plan.clone())
    }

    #[cfg(test)]
    pub(crate) fn active_major_mark_reclaim_prepared(&self) -> bool {
        self.major_mark_state
            .as_ref()
            .is_some_and(|state| state.reclaim_prepared)
    }

    #[cfg(test)]
    pub(crate) fn active_major_mark_has_prepared_reclaim(&self) -> bool {
        self.major_mark_state
            .as_ref()
            .is_some_and(|state| state.prepared_reclaim.is_some())
    }

    pub(crate) fn active_major_mark_ephemerons_processed(&self) -> bool {
        self.major_mark_state
            .as_ref()
            .is_some_and(|state| state.ephemerons_processed)
    }

    pub(crate) fn has_prepared_full_reclaim(&self) -> bool {
        self.major_mark_state.as_ref().is_some_and(|state| {
            state.plan.kind == crate::plan::CollectionKind::Full && state.reclaim_prepared
        })
    }

    pub(crate) fn complete_active_major_remark(
        &mut self,
        mark_steps_delta: u64,
        mark_rounds_delta: u64,
    ) -> bool {
        let Some(state) = self.major_mark_state.as_mut() else {
            return false;
        };
        if !state.worklist.is_empty() {
            return false;
        }
        state.mark_elapsed_nanos = saturating_duration_nanos(state.mark_started_at.elapsed());
        state.mark_steps = state.mark_steps.saturating_add(mark_steps_delta);
        state.mark_rounds = state.mark_rounds.saturating_add(mark_rounds_delta);
        state.ephemerons_processed = true;
        true
    }

    pub(crate) fn complete_active_major_reclaim_prep(
        &mut self,
        mark_steps_delta: u64,
        mark_rounds_delta: u64,
        reclaim_prepare_time: Duration,
        prepared_reclaim: PreparedReclaim,
    ) -> bool {
        let Some(state) = self.major_mark_state.as_mut() else {
            return false;
        };
        if !state.worklist.is_empty() {
            return false;
        }
        state.mark_elapsed_nanos = saturating_duration_nanos(state.mark_started_at.elapsed());
        state.mark_steps = state.mark_steps.saturating_add(mark_steps_delta);
        state.mark_rounds = state.mark_rounds.saturating_add(mark_rounds_delta);
        state.reclaim_prepare_nanos = saturating_duration_nanos(reclaim_prepare_time);
        state.ephemerons_processed = true;
        state.reclaim_prepared = true;
        state.prepared_reclaim = Some(prepared_reclaim);
        true
    }

    pub(crate) fn update_active_major_mark(
        &mut self,
        update: impl FnOnce(&CollectionPlan, MarkWorklist<usize>) -> MajorMarkUpdate,
    ) -> Result<MajorMarkProgress, AllocError> {
        let Some(mut state) = self.major_mark_state.take() else {
            return Err(AllocError::NoCollectionInProgress);
        };

        let update = update(&state.plan, state.worklist);
        state.worklist = update.worklist;
        state.mark_elapsed_nanos = saturating_duration_nanos(state.mark_started_at.elapsed());
        state.mark_steps = state.mark_steps.saturating_add(update.mark_steps_delta);
        state.mark_rounds = state.mark_rounds.saturating_add(update.mark_rounds_delta);
        if !state.worklist.is_empty() {
            state.reclaim_prepare_nanos = 0;
            state.ephemerons_processed = false;
            state.reclaim_prepared = false;
            state.prepared_reclaim = None;
        }

        let progress = MajorMarkProgress {
            completed: state.worklist.is_empty(),
            drained_objects: update.drained_objects,
            elapsed_nanos: state.mark_elapsed_nanos,
            mark_steps: state.mark_steps,
            mark_rounds: state.mark_rounds,
            remaining_work: state.worklist.len(),
        };

        self.major_mark_state = Some(state);
        Ok(progress)
    }

    pub(crate) fn recommended_plan(&self) -> CollectionPlan {
        self.cached_recommended_plan.clone()
    }

    pub(crate) fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.cached_recommended_background_plan.clone()
    }

    pub(crate) fn set_cached_plans(
        &mut self,
        recommended_plan: CollectionPlan,
        recommended_background_plan: Option<CollectionPlan>,
    ) {
        self.cached_recommended_plan = recommended_plan;
        self.cached_recommended_background_plan = recommended_background_plan;
    }

    pub(crate) fn shared_snapshot(&self) -> CollectorSharedSnapshot {
        CollectorSharedSnapshot {
            recommended_plan: self.recommended_plan(),
            recommended_background_plan: self.recommended_background_plan(),
            last_completed_plan: self.last_completed_plan(),
            active_major_mark_plan: self.active_major_mark_plan(),
            major_mark_progress: self.major_mark_progress(),
        }
    }
}

fn saturating_duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
#[path = "collector_state_test.rs"]
mod tests;
