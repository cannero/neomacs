use std::sync::{Arc, Mutex, MutexGuard, TryLockError, TryLockResult};
use std::time::{Duration, Instant};

use crate::heap::AllocError;
use crate::mark::MarkWorklist;
use crate::plan::{CollectionPhase, CollectionPlan, MajorMarkProgress};
use crate::reclaim::PreparedReclaim;

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

    pub(crate) fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.lock().last_completed_plan()
    }

    pub(crate) fn set_last_completed_plan(&self, plan: Option<CollectionPlan>) {
        self.with_state(|state| state.set_last_completed_plan(plan));
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

    pub(crate) fn recommended_plan(&self) -> CollectionPlan {
        self.lock().recommended_plan()
    }

    pub(crate) fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.lock().recommended_background_plan()
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
