use crate::collector_state::CollectorState;
use crate::descriptor::{MovePolicy, TypeDesc};
use crate::heap::HeapConfig;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, OldGenConfig, OldGenState, PinnedSpaceConfig,
};
use crate::stats::HeapStats;

pub(crate) fn build_plan(
    kind: CollectionKind,
    objects: &[ObjectRecord],
    stats: &HeapStats,
    nursery_config: &NurseryConfig,
    old_config: &OldGenConfig,
    old_gen: &OldGenState,
) -> CollectionPlan {
    match kind {
        CollectionKind::Minor => {
            let worker_count = nursery_config.parallel_minor_workers.max(1);
            let mark_slice_budget = objects
                .iter()
                .filter(|object| object.space() == SpaceKind::Nursery)
                .count()
                .max(1)
                .div_ceil(worker_count);
            CollectionPlan {
                kind,
                phase: crate::plan::CollectionPhase::Evacuate,
                concurrent: false,
                parallel: true,
                worker_count,
                mark_slice_budget,
                target_old_regions: 0,
                selected_old_blocks: Vec::new(),
                estimated_compaction_bytes: 0,
                estimated_reclaim_bytes: stats.nursery.live_bytes,
            }
        }
        CollectionKind::Major | CollectionKind::Full => {
            // Block-indexed compaction selection. Runs the
            // hole-bytes heuristic against the per-block view;
            // the runtime feeds selected_old_blocks directly to
            // Heap::compact_old_gen_blocks at major-cycle commit.
            let old_selection = old_gen.block_plan_selection(old_config);
            let selected_old_blocks: Vec<_> = old_selection
                .candidates
                .iter()
                .map(|block| block.region_index)
                .collect();
            let target_old_regions = selected_old_blocks.len();
            let estimated_compaction_bytes = old_selection.estimated_compaction_bytes;
            let old_reclaim_bytes = old_selection.estimated_reclaim_bytes;
            let worker_count = old_config.concurrent_mark_workers.max(1);
            let mark_slice_budget = objects.len().max(1).div_ceil(worker_count);
            let estimated_reclaim_bytes = match kind {
                CollectionKind::Major => old_reclaim_bytes,
                CollectionKind::Full => old_reclaim_bytes
                    .saturating_add(stats.nursery.live_bytes)
                    .saturating_add(stats.large.live_bytes),
                CollectionKind::Minor => unreachable!(),
            };
            CollectionPlan {
                kind,
                phase: crate::plan::CollectionPhase::InitialMark,
                concurrent: old_config.concurrent_mark_workers > 1,
                parallel: true,
                worker_count,
                mark_slice_budget,
                target_old_regions,
                selected_old_blocks,
                estimated_compaction_bytes,
                estimated_reclaim_bytes,
            }
        }
    }
}

pub(crate) fn select_allocation_space(
    config: &HeapConfig,
    desc: &'static TypeDesc,
    payload_bytes: usize,
) -> SpaceKind {
    match desc.move_policy {
        MovePolicy::Pinned => SpaceKind::Pinned,
        MovePolicy::LargeObject => SpaceKind::Large,
        MovePolicy::Immortal => SpaceKind::Immortal,
        MovePolicy::Movable => {
            if payload_bytes >= config.large.threshold_bytes {
                return SpaceKind::Large;
            }
            if payload_bytes > config.nursery.max_regular_object_bytes {
                return SpaceKind::Old;
            }
            SpaceKind::Nursery
        }
        MovePolicy::PromoteToPinned => {
            if payload_bytes >= config.large.threshold_bytes {
                return SpaceKind::Large;
            }
            if payload_bytes > config.nursery.max_regular_object_bytes {
                return SpaceKind::Pinned;
            }
            SpaceKind::Nursery
        }
    }
}

pub(crate) fn allocation_pressure_plan(
    stats: &HeapStats,
    nursery_config: &NurseryConfig,
    pinned_config: &PinnedSpaceConfig,
    large_config: &LargeObjectSpaceConfig,
    space: SpaceKind,
    bytes: usize,
    mut plan_for: impl FnMut(CollectionKind) -> CollectionPlan,
) -> Option<CollectionPlan> {
    match space {
        SpaceKind::Nursery
            if stats.nursery.live_bytes.saturating_add(bytes) > nursery_config.semispace_bytes =>
        {
            Some(plan_for(CollectionKind::Minor))
        }
        SpaceKind::Pinned
            if stats.pinned.live_bytes.saturating_add(bytes) > pinned_config.reserved_bytes =>
        {
            Some(plan_for(CollectionKind::Major))
        }
        SpaceKind::Large
            if stats.large.live_bytes.saturating_add(bytes) > large_config.soft_limit_bytes =>
        {
            Some(plan_for(CollectionKind::Full))
        }
        SpaceKind::Old
        | SpaceKind::Pinned
        | SpaceKind::Large
        | SpaceKind::Nursery
        | SpaceKind::Immortal => None,
    }
}

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
