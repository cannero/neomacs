use super::*;
use crate::descriptor::{Trace, Tracer, fixed_type_desc};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use std::collections::HashSet;

#[derive(Debug)]
struct OldLeaf;

unsafe impl Trace for OldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn crate::descriptor::Relocator) {}
}

fn old_leaf_desc() -> &'static crate::descriptor::TypeDesc {
    Box::leak(Box::new(fixed_type_desc::<OldLeaf>()))
}

#[test]
fn old_gen_record_allocated_object_sets_placement_and_live_stats() {
    let mut object =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate object");
    let mut old_gen = OldGenState::default();
    let config = OldGenConfig::default();

    let reserved_bytes = old_gen.record_allocated_object(&config, &mut object);

    let placement = object
        .old_region_placement()
        .expect("old object placement recorded");
    assert_eq!(placement.region_index, 0);
    assert_eq!(reserved_bytes, old_gen.reserved_bytes());
    assert_eq!(old_gen.regions.len(), 1);
    assert_eq!(old_gen.regions[0].live_bytes, object.total_size());
    assert_eq!(old_gen.regions[0].object_count, 1);
}

#[test]
fn prepare_reclaim_survivor_reassigns_selected_region_after_preserved_regions() {
    let mut first =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate first");
    let mut second =
        ObjectRecord::allocate(old_leaf_desc(), SpaceKind::Old, OldLeaf).expect("allocate second");
    let mut config = OldGenConfig::default();
    config.region_bytes = first.total_size();

    let mut old_gen = OldGenState::default();
    old_gen.record_allocated_object(&config, &mut first);
    old_gen.record_allocated_object(&config, &mut second);

    let plan = CollectionPlan {
        kind: CollectionKind::Major,
        phase: CollectionPhase::Reclaim,
        concurrent: true,
        parallel: true,
        worker_count: 1,
        mark_slice_budget: 1,
        target_old_regions: 1,
        selected_old_regions: vec![0],
        estimated_compaction_bytes: first.total_size(),
        estimated_reclaim_bytes: 0,
    };
    let mut rebuild = old_gen.prepare_rebuild_for_plan(&plan);

    let placement = OldGenState::prepare_reclaim_survivor(
        &mut rebuild,
        &config,
        first.old_region_placement().expect("old placement"),
        first.total_size(),
    )
    .expect("selected old survivor should get rebuilt placement");

    assert_eq!(placement.region_index, 1);
    assert_eq!(rebuild.rebuilt_regions.len(), 1);
    assert_eq!(rebuild.compacted_regions.len(), 1);
    assert_eq!(rebuild.compacted_regions[0].object_count, 1);
    assert_eq!(rebuild.compacted_regions[0].live_bytes, first.total_size());
}

#[test]
fn apply_prepared_reclaim_replaces_regions_and_returns_region_stats() {
    let prepared = PreparedOldGenReclaim {
        rebuilt_regions: vec![OldRegion {
            capacity_bytes: 64,
            used_bytes: 32,
            live_bytes: 24,
            object_count: 2,
            occupied_lines: HashSet::new(),
        }],
        reserved_bytes: 64,
        region_stats: OldRegionCollectionStats {
            compacted_regions: 1,
            reclaimed_regions: 2,
        },
    };
    let mut old_gen = OldGenState::default();

    let stats = old_gen.apply_prepared_reclaim(prepared);

    assert_eq!(
        stats,
        OldRegionCollectionStats {
            compacted_regions: 1,
            reclaimed_regions: 2,
        }
    );
    assert_eq!(old_gen.regions.len(), 1);
    assert_eq!(old_gen.reserved_bytes(), 64);
    assert_eq!(old_gen.regions[0].live_bytes, 24);
}
