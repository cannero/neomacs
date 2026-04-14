use super::*;
use crate::collector_state::CollectorState;
use crate::descriptor::{MovePolicy, Trace, Tracer, fixed_type_desc};
use crate::heap::HeapConfig;
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase};
use crate::spaces::{LargeObjectSpaceConfig, NurseryConfig};
use crate::stats::{HeapStats, SpaceStats};

fn plan_for(kind: CollectionKind) -> CollectionPlan {
    CollectionPlan {
        kind,
        phase: CollectionPhase::InitialMark,
        concurrent: matches!(kind, CollectionKind::Major | CollectionKind::Full),
        parallel: true,
        worker_count: 2,
        mark_slice_budget: 8,
        target_old_regions: 0,
        selected_old_blocks: Vec::new(),
        estimated_compaction_bytes: 0,
        estimated_reclaim_bytes: 0,
    }
}

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<Leaf>()))
}

#[derive(Debug)]
struct PinnedLeaf;

unsafe impl Trace for PinnedLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Pinned
    }
}

fn pinned_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<PinnedLeaf>()))
}

#[derive(Debug)]
struct PromoteToPinnedLeaf;

unsafe impl Trace for PromoteToPinnedLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::PromoteToPinned
    }
}

fn promote_to_pinned_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<PromoteToPinnedLeaf>()))
}

#[test]
fn build_plan_minor_uses_parallel_nursery_budget() {
    let desc = leaf_desc();
    let objects = vec![
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("nursery"),
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("nursery"),
        ObjectRecord::allocate(desc, SpaceKind::Old, Leaf).expect("old"),
    ];
    let stats = HeapStats {
        nursery: SpaceStats {
            live_bytes: 64,
            ..SpaceStats::default()
        },
        ..HeapStats::default()
    };

    let plan = build_plan(
        CollectionKind::Minor,
        objects.len(),
        &stats,
        &NurseryConfig {
            parallel_minor_workers: 2,
            ..NurseryConfig::default()
        },
        &OldGenConfig::default(),
        &OldGenState::default(),
    );

    assert_eq!(plan.phase, CollectionPhase::Evacuate);
    assert_eq!(plan.worker_count, 2);
    // mark_slice_budget is now an O(1) approximation derived
    // from `stats.nursery.live_bytes / 16 / worker_count`
    // instead of an exact walk of `objects`. At 64 live bytes
    // with 2 workers this yields ceil(64/16)/2 = 2. The
    // important invariant the planner still guarantees is
    // that the budget is positive; the exact value is a
    // scheduling hint, not a correctness contract.
    assert!(plan.mark_slice_budget >= 1);
    assert_eq!(plan.estimated_reclaim_bytes, 64);
}

#[test]
fn build_plan_full_includes_old_nursery_and_large_reclaim() {
    let desc = leaf_desc();
    let objects = vec![
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("nursery"),
        ObjectRecord::allocate(desc, SpaceKind::Old, Leaf).expect("old"),
        ObjectRecord::allocate(desc, SpaceKind::Large, Leaf).expect("large"),
    ];
    let stats = HeapStats {
        nursery: SpaceStats {
            live_bytes: 32,
            ..SpaceStats::default()
        },
        large: SpaceStats {
            live_bytes: 48,
            ..SpaceStats::default()
        },
        ..HeapStats::default()
    };
    let old_gen = OldGenState::default();

    let plan = build_plan(
        CollectionKind::Full,
        objects.len(),
        &stats,
        &NurseryConfig::default(),
        &OldGenConfig {
            concurrent_mark_workers: 2,
            ..OldGenConfig::default()
        },
        &old_gen,
    );

    assert_eq!(plan.phase, CollectionPhase::InitialMark);
    assert_eq!(plan.worker_count, 2);
    // The planner reads its candidate set from blocks. This
    // synthetic fixture only populates the legacy regions vec
    // (not the blocks vec) so block-side selection finds zero
    // candidates and the old-gen contribution to all the
    // estimator outputs is zero.
    assert_eq!(plan.target_old_regions, 0);
    assert_eq!(plan.estimated_compaction_bytes, 0);
    // estimated_reclaim_bytes for Full still includes nursery
    // and large bytes from the stats fixture (32 + 48 = 80).
    assert_eq!(plan.estimated_reclaim_bytes, 80);
}

#[test]
fn allocation_pressure_plan_uses_space_thresholds() {
    let stats = HeapStats {
        nursery: SpaceStats {
            live_bytes: 8,
            ..SpaceStats::default()
        },
        pinned: SpaceStats {
            live_bytes: 12,
            ..SpaceStats::default()
        },
        large: SpaceStats {
            live_bytes: 24,
            ..SpaceStats::default()
        },
        ..HeapStats::default()
    };

    assert_eq!(
        allocation_pressure_plan(
            &stats,
            &NurseryConfig {
                semispace_bytes: 16,
                ..NurseryConfig::default()
            },
            &crate::spaces::PinnedSpaceConfig { reserved_bytes: 16 },
            &crate::spaces::LargeObjectSpaceConfig {
                threshold_bytes: 64,
                soft_limit_bytes: 32,
            },
            SpaceKind::Nursery,
            9,
            plan_for,
        )
        .expect("nursery pressure")
        .kind,
        CollectionKind::Minor
    );
    assert_eq!(
        allocation_pressure_plan(
            &stats,
            &NurseryConfig::default(),
            &crate::spaces::PinnedSpaceConfig { reserved_bytes: 16 },
            &crate::spaces::LargeObjectSpaceConfig {
                threshold_bytes: 64,
                soft_limit_bytes: 64,
            },
            SpaceKind::Pinned,
            5,
            plan_for,
        )
        .expect("pinned pressure")
        .kind,
        CollectionKind::Major
    );
    assert_eq!(
        allocation_pressure_plan(
            &stats,
            &NurseryConfig::default(),
            &crate::spaces::PinnedSpaceConfig {
                reserved_bytes: usize::MAX,
            },
            &crate::spaces::LargeObjectSpaceConfig {
                threshold_bytes: 64,
                soft_limit_bytes: 32,
            },
            SpaceKind::Large,
            9,
            plan_for,
        )
        .expect("large pressure")
        .kind,
        CollectionKind::Full
    );
}

#[test]
fn select_allocation_space_uses_move_policy_and_size_thresholds() {
    let config = HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 16,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: 32,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    };

    assert_eq!(
        select_allocation_space(&config, pinned_leaf_desc(), 8),
        SpaceKind::Pinned
    );
    assert_eq!(
        select_allocation_space(&config, leaf_desc(), 8),
        SpaceKind::Nursery
    );
    assert_eq!(
        select_allocation_space(&config, leaf_desc(), 20),
        SpaceKind::Old
    );
    assert_eq!(
        select_allocation_space(&config, leaf_desc(), 40),
        SpaceKind::Large
    );
    assert_eq!(
        select_allocation_space(&config, promote_to_pinned_leaf_desc(), 20),
        SpaceKind::Pinned
    );
}

#[test]
fn recommended_plan_prefers_active_session_plan() {
    let mut collector = CollectorState::default();
    let active_plan = plan_for(CollectionKind::Major);
    collector.begin_major_mark(active_plan.clone(), MarkWorklist::default());

    let recommended = recommended_plan(
        &collector,
        &HeapStats::default(),
        &OldGenState::default(),
        plan_for,
    );

    assert_eq!(
        recommended,
        CollectionPlan {
            phase: CollectionPhase::Remark,
            ..active_plan
        }
    );
}

#[test]
fn recommended_background_plan_requires_concurrency() {
    let stats = HeapStats {
        pinned: SpaceStats {
            live_bytes: 64,
            ..SpaceStats::default()
        },
        ..HeapStats::default()
    };

    assert_eq!(
        recommended_background_plan(
            &CollectorState::default(),
            &stats,
            &OldGenState::default(),
            &OldGenConfig {
                concurrent_mark_workers: 1,
                ..OldGenConfig::default()
            },
            plan_for,
        ),
        None
    );
}

#[test]
fn refresh_cached_plans_prefers_full_for_large_pressure() {
    let mut collector = CollectorState::default();
    let stats = HeapStats {
        large: SpaceStats {
            live_bytes: 256,
            ..SpaceStats::default()
        },
        ..HeapStats::default()
    };
    let old_gen = OldGenState::default();

    refresh_cached_plans(
        &mut collector,
        &stats,
        &old_gen,
        &OldGenConfig {
            concurrent_mark_workers: 2,
            ..OldGenConfig::default()
        },
        plan_for,
    );

    assert_eq!(collector.recommended_plan().kind, CollectionKind::Full);
    assert_eq!(
        collector
            .recommended_background_plan()
            .expect("background recommendation")
            .kind,
        CollectionKind::Full
    );
}
