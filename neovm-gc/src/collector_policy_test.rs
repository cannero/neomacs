use super::*;
use crate::collector_state::CollectorState;
use crate::mark::MarkWorklist;
use crate::plan::{CollectionKind, CollectionPhase};
use crate::spaces::OldRegion;
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
