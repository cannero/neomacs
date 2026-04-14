use super::*;
use crate::descriptor::{Trace, fixed_type_desc};
use crate::index_state::{HeapIndexState, ObjectIndex, ObjectLocator};
use crate::object_store::FlatReadView;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::root::RootStack;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{NurseryConfig, NurseryState, OldGenConfig, OldGenState};
use crate::stats::{HeapStats, SpaceStats};

#[derive(Debug)]
struct Leaf;

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

fn object_index_for(objects: &[ObjectRecord]) -> ObjectIndex {
    objects
        .iter()
        .enumerate()
        .map(|(index, object)| (object.object_key(), ObjectLocator::flat(index)))
        .collect()
}

#[test]
fn trace_major_marks_seeded_source() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let source = object.erased();
    let objects = vec![object];
    let indexes = HeapIndexState {
        object_index: object_index_for(&objects),
        ..HeapIndexState::default()
    };
    let view = FlatReadView::new(&objects, &indexes);

    let (steps, rounds) = super::trace_major(view.raw(), 1, 8, [source]);

    assert_eq!(steps, 1);
    assert_eq!(rounds, 1);
    assert!(objects[0].is_marked());
}

#[test]
fn trace_minor_marks_seeded_nursery_source() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery leaf");
    let source = object.erased();
    let objects = vec![object];
    let indexes = HeapIndexState {
        object_index: object_index_for(&objects),
        ..HeapIndexState::default()
    };
    let view = FlatReadView::new(&objects, &indexes);

    let (steps, rounds) = super::trace_minor(view.raw(), &[], &[], 1, 8, [source]);

    assert_eq!(steps, 1);
    assert_eq!(rounds, 1);
    assert!(objects[0].is_marked());
}

#[test]
fn trace_collection_records_major_phases() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate pinned leaf");
    let source = object.erased();
    let objects = vec![object];
    let indexes = HeapIndexState {
        object_index: object_index_for(&objects),
        ..HeapIndexState::default()
    };
    let mut phases = Vec::new();

    let (steps, rounds) = super::trace_collection(
        &crate::plan::CollectionPlan {
            kind: crate::plan::CollectionKind::Major,
            phase: crate::plan::CollectionPhase::ConcurrentMark,
            concurrent: true,
            parallel: true,
            worker_count: 1,
            mark_slice_budget: 8,
            target_old_regions: 0,
            selected_old_blocks: Vec::new(),
            estimated_compaction_bytes: 0,
            estimated_reclaim_bytes: 0,
        },
        &objects,
        &indexes,
        &[source],
        |phase| phases.push(phase),
    );

    assert_eq!(steps, 1);
    assert_eq!(rounds, 1);
    assert_eq!(
        phases,
        vec![
            crate::plan::CollectionPhase::InitialMark,
            crate::plan::CollectionPhase::ConcurrentMark,
            crate::plan::CollectionPhase::Remark,
        ]
    );
}

#[test]
fn execute_collection_plan_records_minor_phases() {
    // `nursery_state` must be declared BEFORE `objects` so that Rust's
    // reverse-declaration-order local drops release the Vec<ObjectRecord>
    // first, giving arena-backed records a chance to run their
    // drop_in_place before the backing arena buffer is freed.
    let nursery = NurseryConfig::default();
    let mut nursery_state = NurseryState::new(nursery.semispace_bytes);
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let object =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery leaf");
    let object_size = object.total_size();
    let source = object.erased();
    let mut objects = vec![object];
    let mut indexes = HeapIndexState {
        object_index: object_index_for(&objects),
        ..HeapIndexState::default()
    };
    let mut roots = RootStack::default();
    roots.push(source);
    let mut old_gen = OldGenState::default();
    let old = OldGenConfig::default();
    let mut stats = HeapStats {
        nursery: SpaceStats {
            reserved_bytes: nursery.semispace_bytes.saturating_mul(2),
            live_bytes: object_size,
        },
        ..HeapStats::default()
    };
    let runtime_state = RuntimeStateHandle::default();
    let mut phases = Vec::new();

    let cycle = execute_collection_plan(
        &CollectionPlan {
            kind: CollectionKind::Minor,
            phase: CollectionPhase::InitialMark,
            concurrent: false,
            parallel: true,
            worker_count: 1,
            mark_slice_budget: 8,
            target_old_regions: 0,
            selected_old_blocks: Vec::new(),
            estimated_compaction_bytes: 0,
            estimated_reclaim_bytes: 0,
        },
        &mut roots,
        &mut objects,
        &mut indexes,
        &mut old_gen,
        &old,
        &nursery,
        &mut stats,
        &mut nursery_state,
        &runtime_state,
        |phase| phases.push(phase),
    )
    .expect("minor collection should succeed");

    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        phases,
        vec![CollectionPhase::Evacuate, CollectionPhase::Reclaim]
    );
    assert_eq!(objects.len(), 1);
}

#[test]
fn collect_global_sources_includes_roots_and_immortal_objects() {
    let desc = Box::leak(Box::new(fixed_type_desc::<Leaf>()));
    let rooted =
        ObjectRecord::allocate(desc, SpaceKind::Pinned, Leaf).expect("allocate rooted object");
    let immortal =
        ObjectRecord::allocate(desc, SpaceKind::Immortal, Leaf).expect("allocate immortal object");
    let nursery =
        ObjectRecord::allocate(desc, SpaceKind::Nursery, Leaf).expect("allocate nursery object");
    let rooted_source = rooted.erased();
    let immortal_source = immortal.erased();
    let nursery_source = nursery.erased();
    let objects = vec![rooted, immortal, nursery];
    let mut roots = RootStack::default();
    roots.push(rooted_source);

    let indexes = HeapIndexState {
        object_index: object_index_for(&objects),
        ..HeapIndexState::default()
    };
    let view = FlatReadView::new(&objects, &indexes);
    let sources = super::collect_global_sources(&roots, &view);

    assert!(sources.contains(&rooted_source));
    assert!(sources.contains(&immortal_source));
    assert!(!sources.contains(&nursery_source));
}
