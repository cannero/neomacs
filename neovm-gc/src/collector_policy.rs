use crate::collector_state::CollectorState;
use crate::plan::{CollectionKind, CollectionPlan};
use crate::spaces::{OldGenConfig, OldGenState};
use crate::stats::HeapStats;

pub(crate) fn recommended_plan(
    collector: &CollectorState,
    stats: &HeapStats,
    old_gen: &OldGenState,
    mut plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
) -> CollectionPlan {
    if let Some(plan) = collector.active_major_mark_plan() {
        return plan;
    }
    if stats.nursery.live_bytes > 0 {
        return plan_for(CollectionKind::Minor);
    }
    if stats.large.live_bytes > 0 {
        return plan_for(CollectionKind::Full);
    }
    if !old_gen.is_empty() || stats.pinned.live_bytes > 0 {
        return plan_for(CollectionKind::Major);
    }
    plan_for(CollectionKind::Minor)
}

pub(crate) fn recommended_background_plan(
    collector: &CollectorState,
    stats: &HeapStats,
    old_gen: &OldGenState,
    old_config: &OldGenConfig,
    mut plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
) -> Option<CollectionPlan> {
    if let Some(plan) = collector.active_major_mark_plan() {
        return Some(plan);
    }
    if old_config.concurrent_mark_workers <= 1 {
        return None;
    }
    if stats.large.live_bytes > 0 {
        return Some(plan_for(CollectionKind::Full));
    }
    if !old_gen.is_empty() || stats.pinned.live_bytes > 0 {
        return Some(plan_for(CollectionKind::Major));
    }
    None
}

pub(crate) fn refresh_cached_plans(
    collector: &mut CollectorState,
    stats: &HeapStats,
    old_gen: &OldGenState,
    old_config: &OldGenConfig,
    mut plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
) {
    let recommended_plan = recommended_plan(collector, stats, old_gen, &mut plan_for);
    let recommended_background_plan =
        recommended_background_plan(collector, stats, old_gen, old_config, plan_for);
    collector.set_cached_plans(recommended_plan, recommended_background_plan);
}

#[cfg(test)]
#[path = "collector_policy_test.rs"]
mod tests;
