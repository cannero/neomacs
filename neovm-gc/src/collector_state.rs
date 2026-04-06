use crate::mark::MarkWorklist;
use crate::plan::{CollectionPhase, CollectionPlan, MajorMarkProgress};

#[derive(Debug, Default)]
pub(crate) struct CollectorState {
    recent_phase_trace: Vec<CollectionPhase>,
    last_completed_plan: Option<CollectionPlan>,
    major_mark_state: Option<MajorMarkState>,
    cached_recommended_plan: CollectionPlan,
    cached_recommended_background_plan: Option<CollectionPlan>,
}

#[derive(Debug)]
pub(crate) struct MajorMarkState {
    pub(crate) plan: CollectionPlan,
    pub(crate) worklist: MarkWorklist<usize>,
    pub(crate) mark_steps: u64,
    pub(crate) mark_rounds: u64,
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
                CollectionPhase::Remark
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
                mark_steps: state.mark_steps,
                mark_rounds: state.mark_rounds,
                remaining_work: state.worklist.len(),
            })
    }

    pub(crate) fn has_active_major_mark(&self) -> bool {
        self.major_mark_state.is_some()
    }

    pub(crate) fn active_major_mark_state(&self) -> Option<&MajorMarkState> {
        self.major_mark_state.as_ref()
    }

    pub(crate) fn active_major_mark_state_mut(&mut self) -> Option<&mut MajorMarkState> {
        self.major_mark_state.as_mut()
    }

    pub(crate) fn begin_major_mark(&mut self, state: MajorMarkState) {
        self.major_mark_state = Some(state);
    }

    pub(crate) fn take_major_mark_state(&mut self) -> Option<MajorMarkState> {
        self.major_mark_state.take()
    }

    pub(crate) fn restore_major_mark_state(&mut self, state: MajorMarkState) {
        self.major_mark_state = Some(state);
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
}
