use super::*;
use crate::collector_state::CollectorState;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase};
use crate::spaces::NurseryConfig;
use crate::spaces::old::OldRegion;
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
        selected_old_regions: Vec::new(),
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
        &objects,
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
    assert_eq!(plan.mark_slice_budget, 1);
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
    let old_gen = OldGenState {
        regions: vec![OldRegion {
            capacity_bytes: 256,
            used_bytes: 128,
            live_bytes: 64,
            object_count: 1,
            occupied_lines: Default::default(),
        }],
    };

    let plan = build_plan(
        CollectionKind::Full,
        &objects,
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
    assert_eq!(plan.target_old_regions, 1);
    assert_eq!(plan.selected_old_regions, vec![0]);
    assert_eq!(plan.estimated_compaction_bytes, 64);
    assert_eq!(plan.estimated_reclaim_bytes, 144);
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
    let old_gen = OldGenState {
        regions: vec![OldRegion {
            capacity_bytes: 256,
            used_bytes: 0,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: Default::default(),
        }],
    };

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
