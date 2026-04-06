use super::*;
use crate::object::SpaceKind;
use crate::spaces::{LargeObjectSpaceConfig, NurseryConfig};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Leaf(u64);

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

fn lock_shared_heap_on_other_thread(
    shared: SharedHeap,
) -> (mpsc::SyncSender<()>, thread::JoinHandle<()>) {
    let (locked_tx, locked_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let waiter = thread::spawn(move || {
        let _guard = shared.lock().expect("lock shared heap on helper thread");
        locked_tx
            .send(())
            .expect("signal shared heap write-lock acquisition");
        release_rx
            .recv()
            .expect("wait to release shared heap write lock");
    });
    locked_rx
        .recv()
        .expect("wait for helper thread to hold shared heap write lock");
    (release_tx, waiter)
}

fn read_lock_shared_heap_on_other_thread(
    shared: SharedHeap,
) -> (mpsc::SyncSender<()>, thread::JoinHandle<()>) {
    let (locked_tx, locked_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let waiter = thread::spawn(move || {
        let _guard = shared
            .read()
            .expect("read-lock shared heap on helper thread");
        locked_tx
            .send(())
            .expect("signal shared heap read-lock acquisition");
        release_rx
            .recv()
            .expect("wait to release shared heap read lock");
    });
    locked_rx
        .recv()
        .expect("wait for helper thread to hold shared heap read lock");
    (release_tx, waiter)
}

#[test]
fn heap_is_send_and_sync_after_atomic_metadata_split() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Heap>();
}

#[derive(Debug)]
struct PinnedLeaf(u64);

unsafe impl Trace for PinnedLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Pinned
    }
}

#[derive(Debug)]
struct PromoteToPinnedLeaf(u64);

unsafe impl Trace for PromoteToPinnedLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::PromoteToPinned
    }
}

#[derive(Debug)]
struct OversizePromoteToPinnedLeaf([u8; 32]);

unsafe impl Trace for OversizePromoteToPinnedLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::PromoteToPinned
    }
}

#[derive(Debug)]
struct ImmortalLeaf(u64);

unsafe impl Trace for ImmortalLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Immortal
    }
}

#[derive(Debug)]
struct ImmortalHolder {
    child: EdgeCell<Leaf>,
}

unsafe impl Trace for ImmortalHolder {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.child.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.child.relocate(relocator);
    }

    fn move_policy() -> MovePolicy
    where
        Self: Sized,
    {
        MovePolicy::Immortal
    }
}

#[derive(Debug)]
struct LargeLeaf([u8; 80]);

unsafe impl Trace for LargeLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[derive(Debug)]
struct OldLeaf([u8; 32]);

unsafe impl Trace for OldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[derive(Debug)]
struct TinyOldLeaf([u8; 8]);

unsafe impl Trace for TinyOldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[derive(Debug)]
struct Link {
    label: u64,
    next: EdgeCell<Link>,
}

unsafe impl Trace for Link {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.next.relocate(relocator);
    }
}

#[derive(Debug)]
struct SlowLink {
    delay: Duration,
    next: EdgeCell<SlowLink>,
}

unsafe impl Trace for SlowLink {
    fn trace(&self, tracer: &mut dyn Tracer) {
        thread::sleep(self.delay);
        self.next.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.next.relocate(relocator);
    }
}

#[derive(Debug)]
struct ThreadRecordingLeaf {
    seen_threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
}

unsafe impl Trace for ThreadRecordingLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {
        self.seen_threads
            .lock()
            .expect("record trace thread")
            .insert(thread::current().id());
    }

    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

#[derive(Debug)]
struct WeakHolder {
    label: u64,
    strong: EdgeCell<Leaf>,
    weak: WeakCell<Leaf>,
}

unsafe impl Trace for WeakHolder {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.strong.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.strong.relocate(relocator);
    }

    fn process_weak(&self, processor: &mut dyn WeakProcessor) {
        self.weak.process(processor);
    }
}

#[derive(Debug)]
struct EphemeronHolder {
    label: u64,
    strong: EdgeCell<Leaf>,
    pair: Ephemeron<Leaf, Leaf>,
}

unsafe impl Trace for EphemeronHolder {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.strong.trace(tracer);
    }

    fn relocate(&self, relocator: &mut dyn Relocator) {
        self.strong.relocate(relocator);
    }

    fn process_weak(&self, processor: &mut dyn WeakProcessor) {
        self.pair.process(processor);
    }

    fn visit_ephemerons(&self, visitor: &mut dyn EphemeronVisitor) {
        self.pair.visit(visitor);
    }
}

#[derive(Debug)]
struct ThreadRecordingEphemeronHolder {
    seen_threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
    pair: Ephemeron<Leaf, Leaf>,
}

unsafe impl Trace for ThreadRecordingEphemeronHolder {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn process_weak(&self, processor: &mut dyn WeakProcessor) {
        self.pair.process(processor);
    }

    fn visit_ephemerons(&self, visitor: &mut dyn EphemeronVisitor) {
        self.seen_threads
            .lock()
            .expect("record ephemeron thread")
            .insert(thread::current().id());
        self.pair.visit(visitor);
    }
}

#[derive(Debug)]
struct ThreadRecordingWeakHolder {
    seen_threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
    weak: WeakCell<Leaf>,
}

unsafe impl Trace for ThreadRecordingWeakHolder {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn process_weak(&self, processor: &mut dyn WeakProcessor) {
        self.seen_threads
            .lock()
            .expect("record weak-processing thread")
            .insert(thread::current().id());
        self.weak.process(processor);
    }
}

static MINOR_FINALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);
static MAJOR_FINALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct FinalizableNurseryLeaf(u64);

unsafe impl Trace for FinalizableNurseryLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn finalize(&self) {
        MINOR_FINALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::FINALIZABLE
    }
}

#[derive(Debug)]
struct FinalizableOldLeaf([u8; 32]);

unsafe impl Trace for FinalizableOldLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn finalize(&self) {
        MAJOR_FINALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::FINALIZABLE
    }
}

#[test]
fn heap_constructs_with_empty_scope() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let scope = mutator.handle_scope();
    assert_eq!(scope.slot_count(), 0);
}

#[test]
fn alloc_small_object_into_nursery() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let root = mutator.alloc(&mut scope, Leaf(7)).expect("alloc leaf");

    assert_eq!(scope.slot_count(), 1);
    assert!(mutator.heap().stats().nursery.live_bytes > 0);
    assert_eq!(mutator.heap().stats().pinned.live_bytes, 0);
    assert_eq!(mutator.heap().stats().large.live_bytes, 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0, 7);
}

#[test]
fn alloc_auto_triggers_minor_collection_under_nursery_pressure() {
    let leaf_bytes = estimated_allocation_size::<Leaf>().expect("leaf allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            semispace_bytes: leaf_bytes,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, Leaf(70))
        .expect("alloc first leaf");
    let second = mutator
        .alloc_auto(&mut scope, Leaf(71))
        .expect("alloc second leaf");

    assert_eq!(mutator.heap().stats().collections.minor_collections, 1);
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[CollectionPhase::Evacuate, CollectionPhase::Reclaim]
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 70);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 71);
}

#[test]
fn alloc_auto_triggers_major_collection_under_pinned_pressure() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, PinnedLeaf(72))
        .expect("alloc first pinned leaf");
    let second = mutator
        .alloc_auto(&mut scope, PinnedLeaf(73))
        .expect("alloc second pinned leaf");

    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::Remark,
            CollectionPhase::Reclaim,
        ]
    );
    assert_eq!(
        mutator.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 72);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 73);
}

#[test]
fn alloc_auto_triggers_full_collection_under_large_pressure() {
    let large_bytes = estimated_allocation_size::<LargeLeaf>().expect("large allocation size");
    let mut heap = Heap::new(HeapConfig {
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: large_bytes,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, LargeLeaf([2; 80]))
        .expect("alloc first large leaf");
    let second = mutator
        .alloc_auto(&mut scope, LargeLeaf([3; 80]))
        .expect("alloc second large leaf");

    assert_eq!(
        mutator.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Full)
    );
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::Remark,
            CollectionPhase::Evacuate,
            CollectionPhase::Reclaim,
        ]
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 2);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0[0], 3);
}

#[test]
fn full_collection_evacuates_live_nursery_objects() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            promotion_age: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, Leaf(91))
        .expect("alloc nursery leaf");
    let initial_gc = leaf.as_gc();
    let large = mutator
        .alloc(&mut scope, LargeLeaf([7; 80]))
        .expect("alloc large leaf");

    let cycle = mutator.collect(CollectionKind::Full).expect("full collect");

    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.promoted_bytes > 0);
    assert!(!mutator.heap().contains(initial_gc));
    assert_eq!(mutator.heap().space_of(leaf.as_gc()), Some(SpaceKind::Old));
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 91);
    assert_eq!(unsafe { large.as_gc().as_non_null().as_ref() }.0[0], 7);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert!(mutator.heap().stats().old.live_bytes > 0);
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::Remark,
            CollectionPhase::Evacuate,
            CollectionPhase::Reclaim,
        ]
    );
}

#[test]
fn minor_plan_reports_evacuation_and_nursery_bytes() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator.alloc(&mut scope, Leaf(8)).expect("alloc leaf");

    let plan = mutator.plan_for(CollectionKind::Minor);
    assert_eq!(plan.kind, CollectionKind::Minor);
    assert_eq!(plan.phase, CollectionPhase::Evacuate);
    assert!(!plan.concurrent);
    assert!(plan.parallel);
    assert_eq!(plan.worker_count, 1);
    assert_eq!(plan.target_old_regions, 0);
    assert_eq!(plan.estimated_compaction_bytes, 0);
    assert!(plan.estimated_reclaim_bytes > 0);
}

#[test]
fn minor_plan_uses_configured_parallel_worker_budget() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            parallel_minor_workers: 4,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator.alloc(&mut scope, Leaf(81)).expect("alloc leaf");

    let plan = mutator.plan_for(CollectionKind::Minor);
    assert_eq!(plan.kind, CollectionKind::Minor);
    assert_eq!(plan.worker_count, 4);
    assert!(plan.mark_slice_budget > 0);
}

#[test]
fn recommended_plan_prefers_minor_for_live_nursery() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");

    let plan = mutator.recommended_plan();
    assert_eq!(plan.kind, CollectionKind::Minor);
    assert_eq!(plan.phase, CollectionPhase::Evacuate);
}

#[test]
fn execute_minor_plan_records_phase_trace_and_last_plan() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator.alloc(&mut scope, Leaf(10)).expect("alloc leaf");

    let plan = mutator.recommended_plan();
    let cycle = mutator
        .execute_plan(plan.clone())
        .expect("execute minor plan");
    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[CollectionPhase::Evacuate, CollectionPhase::Reclaim]
    );
    assert_eq!(
        mutator.heap().last_completed_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        })
    );
}

#[test]
fn alloc_pinned_object_into_pinned_space() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let root = mutator
        .alloc(&mut scope, PinnedLeaf(11))
        .expect("alloc pinned leaf");

    assert!(mutator.heap().stats().pinned.live_bytes > 0);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0, 11);
}

#[test]
fn alloc_oversize_promote_to_pinned_object_into_pinned_space() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 8,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let root = mutator
        .alloc(&mut scope, OversizePromoteToPinnedLeaf([9; 32]))
        .expect("alloc oversize promotable pinned leaf");

    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Pinned)
    );
    assert!(mutator.heap().stats().pinned.live_bytes > 0);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], 9);
}

#[test]
fn minor_collection_promotes_promote_to_pinned_object_into_pinned_space() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            promotion_age: 1,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let root = mutator
        .alloc(&mut scope, PromoteToPinnedLeaf(33))
        .expect("alloc promotable pinned leaf");

    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Nursery)
    );
    assert!(mutator.heap().stats().nursery.live_bytes > 0);
    assert_eq!(mutator.heap().stats().pinned.live_bytes, 0);

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect promotable pinned leaf");

    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Pinned)
    );
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert!(mutator.heap().stats().pinned.live_bytes > 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0, 33);
}

#[test]
fn alloc_immortal_object_into_immortal_space() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let root = mutator
        .alloc(&mut scope, ImmortalLeaf(77))
        .expect("alloc immortal leaf");

    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Immortal)
    );
    assert_eq!(mutator.heap().object_count(), 1);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(mutator.heap().stats().old.live_bytes, 0);
    assert_eq!(mutator.heap().stats().pinned.live_bytes, 0);
    assert_eq!(mutator.heap().stats().large.live_bytes, 0);
    assert!(mutator.heap().stats().immortal.live_bytes > 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0, 77);
}

#[test]
fn immortal_object_survives_unrooted_major_collection() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let immortal_gc = {
        let mut scope = mutator.handle_scope();
        let root = mutator
            .alloc(&mut scope, ImmortalLeaf(211))
            .expect("alloc immortal leaf");
        root.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect immortal leaf");

    assert_eq!(cycle.major_collections, 1);
    assert!(mutator.heap().contains(immortal_gc));
    assert_eq!(
        mutator.heap().space_of(immortal_gc),
        Some(SpaceKind::Immortal)
    );
    assert_eq!(unsafe { immortal_gc.as_non_null().as_ref() }.0, 211);
}

#[test]
fn minor_collection_immortal_object_keeps_young_child_alive() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let holder = mutator
        .alloc(
            &mut scope,
            ImmortalHolder {
                child: EdgeCell::default(),
            },
        )
        .expect("alloc immortal holder");

    let child_gc = {
        let mut child_scope = mutator.handle_scope();
        let child = mutator
            .alloc(&mut child_scope, Leaf(314))
            .expect("alloc child leaf");
        mutator.store_edge(&holder, 0, |holder| &holder.child, Some(child.as_gc()));
        child.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect immortal holder");

    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        mutator.heap().space_of(holder.as_gc()),
        Some(SpaceKind::Immortal)
    );
    assert!(mutator.heap().stats().immortal.live_bytes > 0);
    assert_eq!(mutator.heap().remembered_edge_count(), 0);
    assert!(!mutator.heap().contains(child_gc));
    let moved_child = unsafe { holder.as_gc().as_non_null().as_ref() }
        .child
        .get()
        .expect("immortal child");
    assert!(mutator.heap().contains(moved_child));
    assert_eq!(unsafe { moved_child.as_non_null().as_ref() }.0, 314);
}

#[test]
fn active_major_mark_keeps_newly_allocated_unrooted_immortal_object_alive() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let _anchor = mutator
        .alloc(&mut scope, Leaf(9))
        .expect("alloc anchor leaf");

    let plan = mutator.plan_for(CollectionKind::Major);
    mutator
        .begin_major_mark(plan)
        .expect("begin persistent major mark");

    let immortal_gc = {
        let mut immortal_scope = mutator.handle_scope();
        let immortal = mutator
            .alloc(&mut immortal_scope, ImmortalLeaf(401))
            .expect("alloc immortal during active major mark");
        immortal.as_gc()
    };

    while !mutator
        .advance_major_mark()
        .expect("advance persistent major mark")
        .completed
    {}

    let cycle = mutator
        .finish_major_collection()
        .expect("finish persistent major mark");

    assert_eq!(cycle.major_collections, 1);
    assert!(mutator.heap().contains(immortal_gc));
    assert_eq!(
        mutator.heap().space_of(immortal_gc),
        Some(SpaceKind::Immortal)
    );
    assert_eq!(unsafe { immortal_gc.as_non_null().as_ref() }.0, 401);
}

#[test]
fn alloc_large_object_into_large_space() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig::default(),
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();

    let root = mutator
        .alloc(&mut scope, LargeLeaf([1; 80]))
        .expect("alloc large leaf");

    assert!(mutator.heap().stats().large.live_bytes > 0);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], 1);
}

#[test]
fn direct_old_allocation_tracks_old_region_stats() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 128,
            line_bytes: 16,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let root = mutator
        .alloc(&mut scope, OldLeaf([7; 32]))
        .expect("alloc old leaf");

    assert_eq!(mutator.heap().space_of(root.as_gc()), Some(SpaceKind::Old));
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].region_index, 0);
    assert_eq!(regions[0].object_count, 1);
    assert!(regions[0].live_bytes > 0);
    assert!(regions[0].occupied_lines > 0);
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], 7);
}

#[test]
fn major_plan_reports_old_region_targets_and_reclaim_headroom() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([17; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([18; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([19; 32]))
            .expect("alloc third old leaf");
        (first.as_gc(), third.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);

    let plan = mutator.plan_for(CollectionKind::Major);
    let candidates = mutator.heap().major_region_candidates();
    assert_eq!(plan.kind, CollectionKind::Major);
    assert_eq!(plan.phase, CollectionPhase::InitialMark);
    assert!(plan.concurrent);
    assert!(plan.parallel);
    assert_eq!(plan.worker_count, 2);
    assert!(plan.mark_slice_budget > 0);
    assert_eq!(plan.target_old_regions, 1);
    assert_eq!(
        plan.selected_old_regions,
        candidates
            .iter()
            .map(|region| region.region_index)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.estimated_compaction_bytes,
        candidates
            .iter()
            .map(|region| region.live_bytes)
            .sum::<usize>()
    );
    assert_eq!(
        plan.estimated_reclaim_bytes,
        candidates
            .iter()
            .map(|region| region.hole_bytes)
            .sum::<usize>()
    );
    assert!(plan.estimated_reclaim_bytes > 0);
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 17);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 19);
}

#[test]
fn major_plan_reports_zero_compaction_bytes_without_old_region_candidates() {
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let pinned = mutator
        .alloc(&mut scope, PinnedLeaf(19))
        .expect("alloc pinned leaf");

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.kind, CollectionKind::Major);
    assert_eq!(plan.target_old_regions, 0);
    assert_eq!(plan.estimated_compaction_bytes, 0);
    assert_eq!(unsafe { pinned.as_gc().as_non_null().as_ref() }.0, 19);
}

#[test]
fn recommended_plan_prefers_major_for_old_generation_pressure() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, OldLeaf([18; 32]))
        .expect("alloc old leaf");

    let plan = mutator.recommended_plan();
    assert_eq!(plan.kind, CollectionKind::Major);
    assert_eq!(plan.phase, CollectionPhase::InitialMark);
}

#[test]
fn execute_major_plan_records_phase_trace_and_last_plan() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let root = mutator
        .alloc(&mut scope, OldLeaf([19; 32]))
        .expect("alloc old leaf");

    let plan = mutator.plan_for(CollectionKind::Major);
    let cycle = mutator
        .execute_plan(plan.clone())
        .expect("execute major plan");
    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.mark_steps > 0);
    assert!(cycle.mark_rounds > 0);
    assert_eq!(
        mutator.heap().stats().collections.mark_steps,
        cycle.mark_steps
    );
    assert_eq!(
        mutator.heap().stats().collections.mark_rounds,
        cycle.mark_rounds
    );
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::ConcurrentMark,
            CollectionPhase::Remark,
            CollectionPhase::Reclaim,
        ]
    );
    assert_eq!(
        mutator.heap().last_completed_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        })
    );
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], 19);
}

#[test]
fn major_mark_session_uses_multiple_steps_with_tiny_budget() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        let root = mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
        assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], byte);
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    let cycle = mutator
        .execute_plan(plan.clone())
        .expect("execute sliced major plan");
    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.mark_steps > 1);
    assert!(cycle.mark_rounds > 1);
    assert_eq!(mutator.heap().object_count(), 40);
}

#[test]
fn execute_major_plan_uses_worker_count_to_reduce_mark_rounds() {
    fn run_major_cycle(worker_count: usize) -> CollectionStats {
        let mut heap = Heap::new(HeapConfig {
            nursery: NurseryConfig {
                max_regular_object_bytes: 1,
                ..NurseryConfig::default()
            },
            large: LargeObjectSpaceConfig {
                threshold_bytes: usize::MAX,
                ..LargeObjectSpaceConfig::default()
            },
            old: crate::spaces::OldGenConfig {
                region_bytes: 512,
                line_bytes: 16,
                concurrent_mark_workers: worker_count,
                mutator_assist_slices: 0,
                ..crate::spaces::OldGenConfig::default()
            },
            ..HeapConfig::default()
        });
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        for byte in 0..40u8 {
            mutator
                .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }

        let plan = CollectionPlan {
            mark_slice_budget: 1,
            ..mutator.plan_for(CollectionKind::Major)
        };
        mutator.execute_plan(plan).expect("execute major plan")
    }

    let single_worker = run_major_cycle(1);
    let four_workers = run_major_cycle(4);

    assert_eq!(single_worker.mark_steps, four_workers.mark_steps);
    assert!(four_workers.mark_rounds < single_worker.mark_rounds);
    assert_eq!(single_worker.mark_rounds, 40);
    assert_eq!(four_workers.mark_rounds, 10);
}

#[test]
fn execute_major_plan_traces_on_multiple_threads_when_worker_count_is_high() {
    let seen_threads = Arc::new(Mutex::new(HashSet::new()));
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for _ in 0..128usize {
        let _leaf = mutator
            .alloc(
                &mut keep_scope,
                ThreadRecordingLeaf {
                    seen_threads: seen_threads.clone(),
                },
            )
            .expect("alloc recording leaf");
    }

    let mut plan = mutator.plan_for(CollectionKind::Major);
    plan.worker_count = 4;
    plan.mark_slice_budget = 8;

    let cycle = mutator.execute_plan(plan).expect("execute major plan");
    let unique_threads = seen_threads.lock().expect("read trace threads").len();

    assert_eq!(cycle.major_collections, 1);
    assert!(
        unique_threads > 1,
        "expected parallel mark tracing across multiple threads, saw {unique_threads}"
    );
}

#[test]
fn execute_minor_plan_traces_on_multiple_threads_when_worker_count_is_high() {
    let seen_threads = Arc::new(Mutex::new(HashSet::new()));
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            parallel_minor_workers: 4,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for _ in 0..128usize {
        let _leaf = mutator
            .alloc(
                &mut keep_scope,
                ThreadRecordingLeaf {
                    seen_threads: seen_threads.clone(),
                },
            )
            .expect("alloc recording leaf");
    }

    let mut plan = mutator.plan_for(CollectionKind::Minor);
    plan.mark_slice_budget = 8;

    let cycle = mutator.execute_plan(plan).expect("execute minor plan");
    let unique_threads = seen_threads.lock().expect("read trace threads").len();

    assert_eq!(cycle.minor_collections, 1);
    assert!(cycle.mark_rounds > 0);
    assert!(
        unique_threads > 1,
        "expected parallel minor tracing across multiple threads, saw {unique_threads}"
    );
}

#[test]
fn execute_major_plan_visits_ephemerons_on_multiple_threads_when_worker_count_is_high() {
    let seen_threads = Arc::new(Mutex::new(HashSet::new()));
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for index in 0..128u64 {
        let key = mutator
            .alloc(&mut keep_scope, Leaf(index))
            .expect("alloc ephemeron key");
        let value = mutator
            .alloc(&mut keep_scope, Leaf(index + 1_000))
            .expect("alloc ephemeron value");
        let _holder = mutator
            .alloc(
                &mut keep_scope,
                ThreadRecordingEphemeronHolder {
                    seen_threads: seen_threads.clone(),
                    pair: Ephemeron::new(Weak::new(key.as_gc()), Weak::new(value.as_gc())),
                },
            )
            .expect("alloc ephemeron holder");
    }

    let mut plan = mutator.plan_for(CollectionKind::Major);
    plan.worker_count = 4;
    plan.mark_slice_budget = 8;

    let cycle = mutator.execute_plan(plan).expect("execute major plan");
    let unique_threads = seen_threads.lock().expect("read ephemeron threads").len();

    assert_eq!(cycle.major_collections, 1);
    assert!(
        unique_threads > 1,
        "expected parallel ephemeron visitation across multiple threads, saw {unique_threads}"
    );
}

#[test]
fn execute_major_plan_processes_weak_edges_on_multiple_threads_when_worker_count_is_high() {
    let seen_threads = Arc::new(Mutex::new(HashSet::new()));
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for index in 0..128u64 {
        let target = mutator
            .alloc(&mut keep_scope, Leaf(index))
            .expect("alloc weak target");
        let _holder = mutator
            .alloc(
                &mut keep_scope,
                ThreadRecordingWeakHolder {
                    seen_threads: seen_threads.clone(),
                    weak: WeakCell::new(Weak::new(target.as_gc())),
                },
            )
            .expect("alloc weak holder");
    }

    let mut plan = mutator.plan_for(CollectionKind::Major);
    plan.worker_count = 4;
    plan.mark_slice_budget = 8;

    let cycle = mutator.execute_plan(plan).expect("execute major plan");
    let unique_threads = seen_threads.lock().expect("read weak threads").len();

    assert_eq!(cycle.major_collections, 1);
    assert!(
        unique_threads > 1,
        "expected parallel weak processing across multiple threads, saw {unique_threads}"
    );
}

#[test]
fn persistent_major_mark_session_advances_across_calls() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        let root = mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
        assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0], byte);
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    assert_eq!(
        mutator.collect(CollectionKind::Minor),
        Err(AllocError::CollectionInProgress)
    );

    let added = mutator
        .alloc(&mut keep_scope, OldLeaf([99; 32]))
        .expect("alloc during persistent major mark");
    assert_eq!(unsafe { added.as_gc().as_non_null().as_ref() }.0[0], 99);

    let mut advances = 0usize;
    let final_progress = loop {
        let progress = mutator
            .advance_major_mark()
            .expect("advance persistent major mark");
        advances += 1;
        if progress.completed {
            break progress;
        }
    };

    assert!(advances > 1);
    assert_eq!(final_progress.remaining_work, 0);
    assert!(final_progress.mark_steps > 1);
    assert!(final_progress.mark_rounds > 1);

    let cycle = mutator
        .finish_major_collection()
        .expect("finish persistent major mark");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.mark_steps, final_progress.mark_steps);
    assert_eq!(cycle.mark_rounds, final_progress.mark_rounds);
    assert_eq!(mutator.heap().object_count(), 41);
    assert_eq!(
        mutator.heap().last_completed_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        })
    );
}

#[test]
fn persistent_major_mark_session_root_keeps_existing_object() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let leaf_gc = {
        let mut setup_scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut setup_scope, OldLeaf([17; 32]))
            .expect("alloc old leaf");
        leaf.as_gc()
    };

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let mut keep_scope = mutator.handle_scope();
    let rooted = mutator.root(&mut keep_scope, leaf_gc);
    assert_eq!(unsafe { rooted.as_gc().as_non_null().as_ref() }.0[0], 17);

    while !mutator
        .advance_major_mark()
        .expect("advance persistent major mark")
        .completed
    {}

    let cycle = mutator
        .finish_major_collection()
        .expect("finish persistent major mark");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 1);
    assert_eq!(unsafe { rooted.as_gc().as_non_null().as_ref() }.0[0], 17);
}

#[test]
fn persistent_major_mark_session_post_write_barrier_keeps_newly_reachable_object() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let owner = mutator
        .alloc(
            &mut keep_scope,
            Link {
                label: 1,
                next: EdgeCell::new(None),
            },
        )
        .expect("alloc owner");
    let target_gc = {
        let mut temp_scope = mutator.handle_scope();
        let target = mutator
            .alloc(
                &mut temp_scope,
                Link {
                    label: 2,
                    next: EdgeCell::new(None),
                },
            )
            .expect("alloc target");
        target.as_gc()
    };

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");
    mutator.store_edge(&owner, 0, |link| &link.next, Some(target_gc));

    while !mutator
        .advance_major_mark()
        .expect("advance persistent major mark")
        .completed
    {}

    let cycle = mutator
        .finish_major_collection()
        .expect("finish persistent major mark");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 2);
    let next = unsafe { owner.as_gc().as_non_null().as_ref() }
        .next
        .get()
        .expect("target retained by barrier");
    assert_eq!(unsafe { next.as_non_null().as_ref() }.label, 2);
    assert!(
        mutator
            .heap()
            .recent_barrier_events()
            .iter()
            .any(|event| event.kind == BarrierKind::PostWrite)
    );
}

#[test]
fn persistent_major_mark_session_satb_keeps_overwritten_snapshot_edge() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let owner = mutator
        .alloc(
            &mut keep_scope,
            Link {
                label: 1,
                next: EdgeCell::new(None),
            },
        )
        .expect("alloc owner");
    let target_gc = {
        let mut temp_scope = mutator.handle_scope();
        let target = mutator
            .alloc(
                &mut temp_scope,
                Link {
                    label: 2,
                    next: EdgeCell::new(None),
                },
            )
            .expect("alloc target");
        target.as_gc()
    };
    mutator.store_edge(&owner, 0, |link| &link.next, Some(target_gc));

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");
    mutator.store_edge(&owner, 0, |link| &link.next, None);

    while !mutator
        .advance_major_mark()
        .expect("advance persistent major mark")
        .completed
    {}

    let cycle = mutator
        .finish_major_collection()
        .expect("finish persistent major mark");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 2);
    assert!(
        unsafe { owner.as_gc().as_non_null().as_ref() }
            .next
            .get()
            .is_none()
    );
    assert!(
        mutator
            .heap()
            .recent_barrier_events()
            .iter()
            .any(|event| event.kind == BarrierKind::SatbPreWrite)
    );
}

#[test]
fn active_major_mark_plan_is_visible_through_recommended_plan() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..12u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    assert_eq!(
        mutator.active_major_mark_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan.clone()
        })
    );
    assert_eq!(
        mutator.recommended_plan(),
        CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan
        }
    );
    assert_eq!(
        mutator.major_mark_progress(),
        Some(MajorMarkProgress {
            completed: false,
            drained_objects: 0,
            mark_steps: 0,
            mark_rounds: 0,
            remaining_work: 12,
        })
    );
}

#[test]
fn allocation_during_active_major_mark_advances_assist_progress() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");
    assert_eq!(
        mutator.major_mark_progress().expect("progress").mark_steps,
        0
    );

    let added = mutator
        .alloc_auto(&mut keep_scope, OldLeaf([99; 32]))
        .expect("alloc during active major mark");
    assert_eq!(unsafe { added.as_gc().as_non_null().as_ref() }.0[0], 99);

    let progress = mutator
        .major_mark_progress()
        .expect("progress after assist");
    assert!(progress.mark_steps > 0);
    assert!(progress.remaining_work > 0);
    assert_eq!(
        mutator.active_major_mark_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan
        })
    );
}

#[test]
fn alloc_auto_starts_concurrent_major_mark_session_under_pinned_pressure() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, PinnedLeaf(72))
        .expect("alloc first pinned leaf");
    let second = mutator
        .alloc_auto(&mut scope, PinnedLeaf(73))
        .expect("alloc second pinned leaf");

    assert_eq!(mutator.heap().stats().collections.major_collections, 0);
    assert_eq!(
        mutator.active_major_mark_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::ConcurrentMark
        ]
    );
    assert!(
        mutator
            .major_mark_progress()
            .expect("active progress")
            .mark_steps
            > 0
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 72);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 73);
}

#[test]
fn poll_active_major_mark_round_and_finish_ready_complete_session() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let first_round = mutator
        .poll_active_major_mark()
        .expect("poll active major mark")
        .expect("active progress");
    assert_eq!(first_round.drained_objects, 2);
    assert!(first_round.mark_steps >= 1);
    assert!(first_round.remaining_work > 0);
    assert_eq!(
        mutator
            .finish_active_major_collection_if_ready()
            .expect("finish if ready"),
        None
    );

    while !mutator
        .poll_active_major_mark()
        .expect("poll active major mark")
        .expect("active progress")
        .completed
    {}

    assert_eq!(
        mutator.active_major_mark_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::Remark,
            ..plan
        })
    );
    let cycle = mutator
        .finish_active_major_collection_if_ready()
        .expect("finish if ready")
        .expect("completed cycle");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 40);
}

#[test]
fn poll_active_major_mark_uses_configured_worker_round_width() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 4,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let first_round = mutator
        .poll_active_major_mark()
        .expect("poll active major mark")
        .expect("active progress");
    assert_eq!(first_round.drained_objects, 4);
    assert_eq!(first_round.mark_steps, 4);
    assert_eq!(first_round.mark_rounds, 1);
    assert!(first_round.remaining_work > 0);
}

#[test]
fn background_collection_round_finishes_active_major_session() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let cycle = loop {
        match mutator
            .service_background_collection_round()
            .expect("service background round")
        {
            BackgroundCollectionStatus::Idle => panic!("session should still be active"),
            BackgroundCollectionStatus::Progress(progress) => {
                assert!(progress.mark_steps > 0);
                assert!(progress.mark_rounds > 0);
            }
            BackgroundCollectionStatus::ReadyToFinish(_) => {
                panic!("direct background service round should finish immediately")
            }
            BackgroundCollectionStatus::Finished(cycle) => break cycle,
        }
    };

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.active_major_mark_plan(), None);
    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 40);
}

#[test]
fn pressure_started_concurrent_session_finishes_via_background_service() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc_auto(&mut scope, PinnedLeaf(72))
        .expect("alloc first pinned leaf");
    mutator
        .alloc_auto(&mut scope, PinnedLeaf(73))
        .expect("alloc second pinned leaf");

    assert_eq!(
        mutator.active_major_mark_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
    assert_eq!(mutator.heap().stats().collections.major_collections, 0);

    let cycle = loop {
        match mutator
            .service_background_collection_round()
            .expect("service background round")
        {
            BackgroundCollectionStatus::Idle => panic!("concurrent session should be active"),
            BackgroundCollectionStatus::Progress(_) => {}
            BackgroundCollectionStatus::ReadyToFinish(_) => {
                panic!("direct background service round should finish immediately")
            }
            BackgroundCollectionStatus::Finished(cycle) => break cycle,
        }
    };

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.active_major_mark_plan(), None);
    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
}

#[test]
fn background_collector_auto_starts_and_finishes_concurrent_major_plan() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..16u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let mut collector = BackgroundCollector::default();
    assert_eq!(collector.stats().sessions_started, 0);

    let cycle = collector
        .run_until_idle(&mut mutator)
        .expect("run background collector")
        .expect("finished cycle");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(collector.stats().sessions_started, 1);
    assert_eq!(collector.stats().sessions_finished, 1);
    assert!(collector.stats().ticks > 0);
    assert!(collector.stats().rounds > 0);
    assert_eq!(mutator.active_major_mark_plan(), None);
}

#[test]
fn background_collector_can_disable_auto_start() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..8u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let mut collector = BackgroundCollector::new(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 1,
    });
    assert_eq!(
        collector
            .tick(&mut mutator)
            .expect("tick background collector"),
        BackgroundCollectionStatus::Idle
    );
    assert_eq!(collector.stats().sessions_started, 0);
    assert_eq!(mutator.active_major_mark_plan(), None);
}

#[test]
fn background_collector_auto_starts_and_finishes_concurrent_full_plan() {
    let mut heap = Heap::new(HeapConfig {
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, LargeLeaf([9; 80]))
        .expect("alloc large leaf");

    let mut collector = BackgroundCollector::default();
    let cycle = collector
        .run_until_idle(&mut mutator)
        .expect("run background collector")
        .expect("finished full cycle");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(
        mutator.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Full)
    );
    assert_eq!(collector.stats().sessions_started, 1);
    assert_eq!(collector.stats().sessions_finished, 1);
}

#[test]
fn recommended_background_plan_prefers_major_even_with_live_nursery() {
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, Leaf(1))
        .expect("alloc nursery leaf");
    mutator
        .alloc(&mut scope, PinnedLeaf(2))
        .expect("alloc pinned leaf");

    assert_eq!(mutator.recommended_plan().kind, CollectionKind::Minor);
    assert_eq!(
        mutator.recommended_background_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
}

#[test]
fn recommended_background_plan_is_none_when_concurrency_is_disabled() {
    let mut heap = Heap::new(HeapConfig {
        pinned: crate::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, PinnedLeaf(3))
        .expect("alloc pinned leaf");

    assert_eq!(mutator.recommended_background_plan(), None);
}

#[test]
fn background_collector_prefers_full_even_with_live_nursery() {
    let mut heap = Heap::new(HeapConfig {
        large: LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, Leaf(3))
        .expect("alloc nursery leaf");
    mutator
        .alloc(&mut scope, LargeLeaf([4; 80]))
        .expect("alloc large leaf");

    let mut collector = BackgroundCollector::default();
    match collector
        .tick(&mut mutator)
        .expect("tick background collector")
    {
        BackgroundCollectionStatus::Idle => panic!("background collector should auto-start"),
        BackgroundCollectionStatus::Progress(progress) => {
            assert!(progress.mark_steps > 0);
        }
        BackgroundCollectionStatus::ReadyToFinish(progress) => {
            assert!(progress.completed);
        }
        BackgroundCollectionStatus::Finished(cycle) => {
            assert_eq!(cycle.major_collections, 1);
        }
    }

    assert!(
        mutator.active_major_mark_plan().map(|plan| plan.kind) == Some(CollectionKind::Full)
            || mutator.heap().last_completed_plan().map(|plan| plan.kind)
                == Some(CollectionKind::Full)
    );
}

#[test]
fn background_collector_tick_aggregates_multiple_rounds() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let mut collector = BackgroundCollector::new(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });
    match collector
        .tick(&mut mutator)
        .expect("tick background collector")
    {
        BackgroundCollectionStatus::Idle => panic!("session should be active"),
        BackgroundCollectionStatus::Finished(_) => {
            panic!("single tick should not finish whole session")
        }
        BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single tick should not drain the whole session")
        }
        BackgroundCollectionStatus::Progress(progress) => {
            assert_eq!(progress.drained_objects, 4);
            assert_eq!(progress.mark_steps, 4);
            assert_eq!(progress.mark_rounds, 2);
            assert!(progress.remaining_work > 0);
        }
    }
}

#[test]
fn background_collector_try_tick_preserves_partial_progress_before_would_block() {
    let mut collector = BackgroundCollector::new(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });
    let mut attempt = 0usize;

    let status = collector.try_tick_with_rounds(|_| {
        attempt = attempt.saturating_add(1);
        match attempt {
            1 => Ok(BackgroundCollectionStatus::Progress(MajorMarkProgress {
                completed: false,
                drained_objects: 2,
                mark_steps: 1,
                mark_rounds: 1,
                remaining_work: 3,
            })),
            2 => Err(SharedBackgroundError::WouldBlock),
            _ => unreachable!("only two attempts expected"),
        }
    });

    match status {
        Ok(BackgroundCollectionStatus::Progress(progress)) => {
            assert_eq!(progress.drained_objects, 2);
            assert_eq!(progress.mark_steps, 1);
            assert_eq!(progress.mark_rounds, 1);
            assert_eq!(progress.remaining_work, 3);
        }
        other => panic!("expected partial progress before contention, got {other:?}"),
    }
    assert_eq!(collector.stats().ticks, 1);
}

#[test]
fn background_collector_try_tick_returns_would_block_without_progress() {
    let mut collector = BackgroundCollector::new(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });

    let status = collector.try_tick_with_rounds(|_| Err(SharedBackgroundError::WouldBlock));

    assert_eq!(status, Err(SharedBackgroundError::WouldBlock));
    assert_eq!(collector.stats().ticks, 1);
}

#[test]
fn background_collector_can_leave_ready_session_for_explicit_finish() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..8u8 {
        mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc old leaf");
    }

    let plan = CollectionPlan {
        mark_slice_budget: usize::MAX,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let mut collector = BackgroundCollector::new(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: false,
        max_rounds_per_tick: 1,
    });
    let progress = match collector
        .tick(&mut mutator)
        .expect("tick background collector")
    {
        BackgroundCollectionStatus::Idle => panic!("session should be active"),
        BackgroundCollectionStatus::Finished(_) => {
            panic!("tick should not auto-finish the ready session")
        }
        BackgroundCollectionStatus::Progress(_) => {
            panic!("tick should expose a ready-to-finish session")
        }
        BackgroundCollectionStatus::ReadyToFinish(progress) => progress,
    };

    assert!(progress.completed);
    assert_eq!(progress.remaining_work, 0);
    assert_eq!(collector.stats().sessions_finished, 0);
    assert!(mutator.active_major_mark_plan().is_some());

    let cycle = mutator
        .finish_active_major_collection_if_ready()
        .expect("finish ready session")
        .expect("completed cycle");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.active_major_mark_plan(), None);
}

#[test]
fn major_region_candidates_respect_limit_and_sort_by_hole_bytes() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: estimated_allocation_size::<OldLeaf>()
                .expect("old allocation size")
                .saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let rooted: Vec<_> = {
        let mut setup_scope = mutator.handle_scope();
        let leaves = [
            mutator
                .alloc(&mut setup_scope, OldLeaf([0; 32]))
                .expect("alloc old leaf 0")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([1; 32]))
                .expect("alloc old leaf 1")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([2; 32]))
                .expect("alloc old leaf 2")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([3; 32]))
                .expect("alloc old leaf 3")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([4; 32]))
                .expect("alloc old leaf 4")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([5; 32]))
                .expect("alloc old leaf 5")
                .as_gc(),
        ];
        vec![leaves[0], leaves[2], leaves[3], leaves[5]]
    };
    for gc in rooted {
        let root = mutator.root(&mut keep_scope, gc);
        assert!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0] <= 5);
    }

    let candidates = mutator.heap().major_region_candidates();
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].hole_bytes >= candidates[1].hole_bytes);
    assert!(candidates.iter().all(|region| region.hole_bytes > 0));

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.target_old_regions, 2);
    assert_eq!(
        plan.estimated_compaction_bytes,
        candidates
            .iter()
            .map(|region| region.live_bytes)
            .sum::<usize>()
    );
    assert!(plan.estimated_reclaim_bytes >= candidates[0].hole_bytes + candidates[1].hole_bytes);
}

#[test]
fn major_region_candidates_prefer_holey_regions_over_tail_only_sparse_regions() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc, fourth_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([80; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([81; 32]))
            .expect("alloc second old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([82; 32]))
            .expect("alloc third old leaf");
        let fourth = mutator
            .alloc(&mut setup_scope, OldLeaf([83; 32]))
            .expect("alloc fourth old leaf");
        (first.as_gc(), third.as_gc(), fourth.as_gc())
    };
    let _first = mutator.root(&mut keep_scope, first_gc);
    let _third = mutator.root(&mut keep_scope, third_gc);
    let _fourth = mutator.root(&mut keep_scope, fourth_gc);
    let tiny = mutator
        .alloc(&mut keep_scope, TinyOldLeaf([84; 8]))
        .expect("alloc tiny tail-only old leaf");
    assert_eq!(unsafe { tiny.as_gc().as_non_null().as_ref() }.0[0], 84);

    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 2);
    let holey_region = regions
        .iter()
        .find(|region| region.hole_bytes > 0)
        .expect("expected a holey region");
    let tail_only_region = regions
        .iter()
        .find(|region| region.hole_bytes == 0 && region.free_bytes > holey_region.free_bytes)
        .expect("expected a tail-only sparse region with more raw free bytes");

    let candidates = mutator.heap().major_region_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].region_index, holey_region.region_index);
    assert!(candidates[0].hole_bytes > 0);

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.selected_old_regions, vec![holey_region.region_index]);
    assert_eq!(plan.target_old_regions, 1);
    assert_eq!(plan.estimated_reclaim_bytes, holey_region.hole_bytes);
    assert!(
        tail_only_region.free_bytes > holey_region.free_bytes,
        "tail-only sparse region should remain more free but no longer be a compaction target"
    );
}

#[test]
fn major_region_candidates_respect_compaction_byte_budget() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            max_compaction_bytes_per_cycle: old_bytes.saturating_mul(3),
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let rooted: Vec<_> = {
        let mut setup_scope = mutator.handle_scope();
        let leaves = [
            mutator
                .alloc(&mut setup_scope, OldLeaf([100; 32]))
                .expect("alloc old leaf 0")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([101; 32]))
                .expect("alloc old leaf 1")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([102; 32]))
                .expect("alloc old leaf 2")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([103; 32]))
                .expect("alloc old leaf 3")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([104; 32]))
                .expect("alloc old leaf 4")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([105; 32]))
                .expect("alloc old leaf 5")
                .as_gc(),
        ];
        vec![leaves[0], leaves[2], leaves[3], leaves[5]]
    };
    for gc in rooted {
        let root = mutator.root(&mut keep_scope, gc);
        assert!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0] >= 100);
    }

    let regions = mutator.heap().old_region_stats();
    let holey_regions: Vec<_> = regions
        .iter()
        .filter(|region| region.hole_bytes > 0)
        .collect();
    assert!(
        holey_regions.len() >= 2,
        "fixture should expose multiple holey regions before budgeting"
    );

    let candidates = mutator.heap().major_region_candidates();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].hole_bytes > 0);
    assert!(candidates[0].live_bytes <= old_bytes.saturating_mul(3));
    assert!(holey_regions.len() > candidates.len());

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.target_old_regions, 1);
    assert_eq!(plan.selected_old_regions, vec![candidates[0].region_index]);
    assert_eq!(plan.estimated_compaction_bytes, candidates[0].live_bytes);
    assert_eq!(plan.estimated_reclaim_bytes, candidates[0].hole_bytes);
}

#[test]
fn major_region_candidates_prefer_more_reclaim_efficient_regions_under_budget() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (a_first, a_third, b_first, b_tiny) = {
        let mut setup_scope = mutator.handle_scope();
        let a_first = mutator
            .alloc(&mut setup_scope, OldLeaf([120; 32]))
            .expect("alloc region-a first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([121; 32]))
            .expect("alloc region-a middle old leaf");
        let a_third = mutator
            .alloc(&mut setup_scope, OldLeaf([122; 32]))
            .expect("alloc region-a third old leaf");
        let b_first = mutator
            .alloc(&mut setup_scope, OldLeaf([123; 32]))
            .expect("alloc region-b first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([124; 32]))
            .expect("alloc region-b middle old leaf");
        let b_tiny = mutator
            .alloc(&mut setup_scope, TinyOldLeaf([125; 8]))
            .expect("alloc region-b tiny tail leaf");
        (
            a_first.as_gc(),
            a_third.as_gc(),
            b_first.as_gc(),
            b_tiny.as_gc(),
        )
    };
    let a_first = mutator.root(&mut keep_scope, a_first);
    let a_third = mutator.root(&mut keep_scope, a_third);
    let b_first = mutator.root(&mut keep_scope, b_first);
    let b_tiny = mutator.root(&mut keep_scope, b_tiny);

    let regions = mutator.heap().old_region_stats();
    let holey_regions: Vec<_> = regions
        .iter()
        .filter(|region| region.hole_bytes > 0)
        .collect();
    assert!(
        holey_regions.len() >= 2,
        "fixture should expose at least two holey regions"
    );
    assert!(
        holey_regions
            .iter()
            .any(|region| region.region_index != holey_regions[0].region_index),
        "fixture should include at least one competing holey region"
    );

    let candidates = mutator.heap().major_region_candidates();
    assert!(
        !candidates.is_empty(),
        "fixture should produce at least one compaction candidate"
    );
    let selected = &candidates[0];
    for region in &holey_regions {
        let selected_score =
            (selected.hole_bytes as u128).saturating_mul(region.live_bytes.max(1) as u128);
        let other_score =
            (region.hole_bytes as u128).saturating_mul(selected.live_bytes.max(1) as u128);
        assert!(
            selected_score >= other_score,
            "selected region should not be less reclaim-efficient than any competing holey region"
        );
    }
    let plan = mutator.plan_for(CollectionKind::Major);
    assert!(
        plan.target_old_regions >= 1,
        "plan should carry at least the most efficient selected region"
    );
    assert_eq!(plan.selected_old_regions[0], selected.region_index);

    assert_eq!(unsafe { a_first.as_gc().as_non_null().as_ref() }.0[0], 120);
    assert_eq!(unsafe { a_third.as_gc().as_non_null().as_ref() }.0[0], 122);
    assert_eq!(unsafe { b_first.as_gc().as_non_null().as_ref() }.0[0], 123);
    assert_eq!(unsafe { b_tiny.as_gc().as_non_null().as_ref() }.0[0], 125);
}

#[test]
fn major_collection_reuses_empty_old_region_for_later_old_allocation() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 1,
            line_bytes: 16,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let second_gc = {
        let mut setup_scope = mutator.handle_scope();
        mutator
            .alloc(&mut setup_scope, OldLeaf([1; 32]))
            .expect("alloc first old leaf");
        let second = mutator
            .alloc(&mut setup_scope, OldLeaf([2; 32]))
            .expect("alloc second old leaf");
        second.as_gc()
    };
    let second = mutator.root(&mut keep_scope, second_gc);
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 2);
    assert_eq!(
        regions
            .iter()
            .map(|region| region.object_count)
            .sum::<usize>(),
        2
    );

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 0);
    assert_eq!(cycle.reclaimed_regions, 1);
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].object_count, 1);

    let third = mutator
        .alloc(&mut keep_scope, OldLeaf([3; 32]))
        .expect("alloc reused old leaf");
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].object_count, 1);
    assert_eq!(regions[1].object_count, 1);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0[0], 2);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 3);
}

#[test]
fn major_collection_repacks_surviving_old_objects_to_drop_interior_holes() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 1,
            line_bytes: 16,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([10; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([11; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([12; 32]))
            .expect("alloc third old leaf");
        (first.as_gc(), third.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);

    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 3);
    assert_eq!(
        regions
            .iter()
            .map(|region| region.object_count)
            .sum::<usize>(),
        3
    );

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 0);
    assert_eq!(cycle.reclaimed_regions, 1);
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 2);
    assert_eq!(
        regions
            .iter()
            .map(|region| region.object_count)
            .sum::<usize>(),
        2
    );
    assert_eq!(regions[0].region_index, 0);
    assert_eq!(regions[1].region_index, 1);
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 10);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 12);
}

#[test]
fn major_collection_preserves_non_candidate_hole_in_live_old_region() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: usize::MAX,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([20; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([21; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([22; 32]))
            .expect("alloc third old leaf");
        (first.as_gc(), third.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 0);
    assert_eq!(cycle.reclaimed_regions, 0);

    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].object_count, 2);
    assert!(regions[0].hole_bytes > 0);
    assert_eq!(
        mutator.heap().major_region_candidates().len(),
        0,
        "threshold should keep the region out of the compaction set"
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 20);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 22);
}

#[test]
fn major_collection_compacts_selected_live_old_region() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: 1,
            compaction_candidate_limit: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([30; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([31; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([32; 32]))
            .expect("alloc third old leaf");
        (first.as_gc(), third.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 1);
    assert_eq!(cycle.reclaimed_regions, 0);

    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].object_count, 2);
    assert!(regions[0].hole_bytes < old_bytes);
    assert!(regions[0].tail_bytes > 0);
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 30);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 32);
}

#[test]
fn execute_major_plan_honors_exact_selected_old_regions() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: 1,
            compaction_candidate_limit: 1,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc, fourth_gc, sixth_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([60; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([61; 32]))
            .expect("alloc second old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([62; 32]))
            .expect("alloc third old leaf");
        let fourth = mutator
            .alloc(&mut setup_scope, OldLeaf([63; 32]))
            .expect("alloc fourth old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([64; 32]))
            .expect("alloc fifth old leaf");
        let sixth = mutator
            .alloc(&mut setup_scope, OldLeaf([65; 32]))
            .expect("alloc sixth old leaf");
        (first.as_gc(), third.as_gc(), fourth.as_gc(), sixth.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);
    let fourth = mutator.root(&mut keep_scope, fourth_gc);
    let sixth = mutator.root(&mut keep_scope, sixth_gc);

    let before_regions = mutator.heap().old_region_stats();
    let candidate_regions: Vec<_> = before_regions
        .iter()
        .filter(|region| region.object_count > 1 && region.hole_bytes > 0)
        .map(|region| region.region_index)
        .collect();
    assert!(
        candidate_regions.len() >= 2,
        "fixture should produce at least two live compaction candidates"
    );

    let planned = mutator.plan_for(CollectionKind::Major);
    assert_eq!(planned.selected_old_regions.len(), 1);
    let manual_selected = *candidate_regions
        .iter()
        .filter(|&&index| !planned.selected_old_regions.contains(&index))
        .max()
        .expect("need a non-default region candidate");
    let preserved_region = *candidate_regions
        .iter()
        .find(|&&index| index != manual_selected)
        .expect("need a preserved candidate region");
    let before_manual = before_regions
        .iter()
        .find(|region| region.region_index == manual_selected)
        .expect("manual region stats");
    let manual_plan = CollectionPlan {
        target_old_regions: 1,
        selected_old_regions: vec![manual_selected],
        estimated_compaction_bytes: before_manual.live_bytes,
        ..planned
    };

    let cycle = mutator
        .execute_plan(manual_plan.clone())
        .expect("execute major plan with explicit region selection");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 1);

    let after_regions = mutator.heap().old_region_stats();
    assert_eq!(after_regions.len(), before_regions.len());
    let after_manual = after_regions
        .iter()
        .find(|region| region.region_index == manual_selected)
        .expect("compacted manual region stats");
    let after_preserved = after_regions
        .iter()
        .find(|region| region.region_index == preserved_region)
        .expect("preserved region stats after manual plan");
    assert!(after_manual.hole_bytes < before_manual.hole_bytes);
    assert!(after_preserved.hole_bytes > 0);
    assert_eq!(
        mutator.heap().last_completed_plan(),
        Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..manual_plan
        })
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 60);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 62);
    assert_eq!(unsafe { fourth.as_gc().as_non_null().as_ref() }.0[0], 63);
    assert_eq!(unsafe { sixth.as_gc().as_non_null().as_ref() }.0[0], 65);
}

#[test]
fn dropping_scope_releases_root_slots() {
    let mut heap = Heap::new(HeapConfig::default());

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        mutator.alloc(&mut scope, Leaf(13)).expect("alloc leaf");
        assert_eq!(scope.slot_count(), 1);
        assert_eq!(mutator.heap().root_slot_count(), 1);
    }

    assert_eq!(heap.root_slot_count(), 0);
}

#[test]
fn major_collection_reclaims_unrooted_objects() {
    let mut heap = Heap::new(HeapConfig::default());

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        mutator.alloc(&mut scope, Leaf(21)).expect("alloc leaf");
        assert_eq!(mutator.heap().object_count(), 1);
    }

    let cycle = heap.collect(CollectionKind::Major).expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.reclaimed_bytes > 0);
    assert_eq!(
        heap.stats().collections.reclaimed_bytes,
        cycle.reclaimed_bytes
    );
    assert_eq!(heap.object_count(), 0);
    assert_eq!(heap.stats().nursery.live_bytes, 0);
}

#[test]
fn minor_collection_finalizes_dead_nursery_object() {
    MINOR_FINALIZE_COUNT.store(0, Ordering::SeqCst);

    let mut heap = Heap::new(HeapConfig::default());
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut scope, FinalizableNurseryLeaf(41))
            .expect("alloc finalizable nursery leaf");
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 41);
    }

    let cycle = heap.collect(CollectionKind::Minor).expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(cycle.finalized_objects, 1);
    assert_eq!(heap.stats().collections.finalized_objects, 1);
    assert_eq!(MINOR_FINALIZE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(heap.object_count(), 0);
}

#[test]
fn major_collection_finalizes_dead_old_object() {
    MAJOR_FINALIZE_COUNT.store(0, Ordering::SeqCst);

    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut scope, FinalizableOldLeaf([42; 32]))
            .expect("alloc finalizable old leaf");
        assert_eq!(mutator.heap().space_of(leaf.as_gc()), Some(SpaceKind::Old));
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 42);
    }

    let cycle = heap.collect(CollectionKind::Major).expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.finalized_objects, 1);
    assert_eq!(heap.stats().collections.finalized_objects, 1);
    assert_eq!(MAJOR_FINALIZE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(heap.object_count(), 0);
}

#[test]
fn major_collection_preserves_rooted_objects() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let root = mutator.alloc(&mut scope, Leaf(34)).expect("alloc leaf");

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 1);
    assert!(mutator.heap().contains(root.as_gc()));
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.0, 34);
}

#[test]
fn major_collection_reclaims_unreachable_cycle() {
    let mut heap = Heap::new(HeapConfig::default());

    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let a = mutator
            .alloc(
                &mut scope,
                Link {
                    label: 1,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc a");
        let b = mutator
            .alloc(
                &mut scope,
                Link {
                    label: 2,
                    next: EdgeCell::new(Some(a.as_gc())),
                },
            )
            .expect("alloc b");
        unsafe {
            a.as_gc().as_non_null().as_ref().next.set(Some(b.as_gc()));
        }
        assert_eq!(mutator.heap().object_count(), 2);
    }

    let cycle = heap.collect(CollectionKind::Major).expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(heap.object_count(), 0);
}

#[test]
fn minor_collection_promotes_reachable_nursery_objects() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let root = mutator
        .alloc(
            &mut scope,
            Link {
                label: 55,
                next: EdgeCell::default(),
            },
        )
        .expect("alloc link");
    let initial_gc = root.as_gc();

    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Nursery)
    );

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(cycle.promoted_bytes, 0);
    assert!(!mutator.heap().contains(initial_gc));
    assert_eq!(
        mutator.heap().space_of(root.as_gc()),
        Some(SpaceKind::Nursery)
    );
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.label, 55);

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(cycle.promoted_bytes > 0);
    assert_eq!(mutator.heap().space_of(root.as_gc()), Some(SpaceKind::Old));
    assert_eq!(unsafe { root.as_gc().as_non_null().as_ref() }.label, 55);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert!(mutator.heap().stats().old.live_bytes > 0);
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].object_count, 1);
    assert!(regions[0].live_bytes > 0);
}

#[test]
fn minor_collection_traces_young_objects_reachable_from_old_objects() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut parent_scope = mutator.handle_scope();
    let parent = mutator
        .alloc(
            &mut parent_scope,
            Link {
                label: 80,
                next: EdgeCell::default(),
            },
        )
        .expect("alloc parent");

    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(parent.as_gc()),
        Some(SpaceKind::Old)
    );

    let child_gc = {
        let mut child_scope = mutator.handle_scope();
        let child = mutator
            .alloc(
                &mut child_scope,
                Link {
                    label: 81,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc child");
        mutator.store_edge(&parent, 0, |link| &link.next, Some(child.as_gc()));
        child.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(!mutator.heap().contains(child_gc));
    let moved_child = unsafe { parent.as_gc().as_non_null().as_ref() }
        .next
        .get()
        .expect("child link");
    assert!(mutator.heap().contains(moved_child));
    assert_eq!(
        mutator.heap().space_of(moved_child),
        Some(SpaceKind::Nursery)
    );
    assert_eq!(mutator.heap().remembered_edge_count(), 1);
    assert!(mutator.heap().barrier_event_count() > 0);
    assert_eq!(
        unsafe { parent.as_gc().as_non_null().as_ref() }
            .next
            .get()
            .expect("child link"),
        moved_child
    );
}

#[test]
fn minor_collection_drops_young_child_without_barrier_on_non_root_old_owner() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut root_scope = mutator.handle_scope();
    let root = mutator
        .alloc(
            &mut root_scope,
            Link {
                label: 90,
                next: EdgeCell::default(),
            },
        )
        .expect("alloc root");
    let stale_mid_gc = {
        let mut mid_scope = mutator.handle_scope();
        let mid = mutator
            .alloc(
                &mut mid_scope,
                Link {
                    label: 91,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc mid");
        mutator.store_edge(&root, 0, |link| &link.next, Some(mid.as_gc()));
        mid.as_gc()
    };
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert!(!mutator.heap().contains(stale_mid_gc));
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    let live_mid_gc = unsafe { root.as_gc().as_non_null().as_ref() }
        .next
        .get()
        .expect("moved mid");
    assert!(mutator.heap().contains(live_mid_gc));
    assert_eq!(mutator.heap().space_of(live_mid_gc), Some(SpaceKind::Old));

    let child_gc = {
        let mut child_scope = mutator.handle_scope();
        let child = mutator
            .alloc(
                &mut child_scope,
                Link {
                    label: 92,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc child");
        let mid = mutator.root(&mut child_scope, live_mid_gc);
        unsafe {
            mid.as_gc()
                .as_non_null()
                .as_ref()
                .next
                .set(Some(child.as_gc()));
        }
        child.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(!mutator.heap().contains(child_gc));
    let stale_edge = unsafe { live_mid_gc.as_non_null().as_ref() }
        .next
        .get()
        .expect("stale child edge");
    assert!(!mutator.heap().contains(stale_edge));
}

#[test]
fn minor_collection_keeps_young_child_with_barrier_on_non_root_old_owner() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut root_scope = mutator.handle_scope();
    let root = mutator
        .alloc(
            &mut root_scope,
            Link {
                label: 100,
                next: EdgeCell::default(),
            },
        )
        .expect("alloc root");
    let stale_mid_gc = {
        let mut mid_scope = mutator.handle_scope();
        let mid = mutator
            .alloc(
                &mut mid_scope,
                Link {
                    label: 101,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc mid");
        mutator.store_edge(&root, 0, |link| &link.next, Some(mid.as_gc()));
        mid.as_gc()
    };
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert!(!mutator.heap().contains(stale_mid_gc));
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    let live_mid_gc = unsafe { root.as_gc().as_non_null().as_ref() }
        .next
        .get()
        .expect("moved mid");
    assert!(mutator.heap().contains(live_mid_gc));
    assert_eq!(mutator.heap().space_of(live_mid_gc), Some(SpaceKind::Old));

    let child_gc = {
        let mut child_scope = mutator.handle_scope();
        let child = mutator
            .alloc(
                &mut child_scope,
                Link {
                    label: 102,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc child");
        let mid = mutator.root(&mut child_scope, live_mid_gc);
        mutator.store_edge(&mid, 0, |link| &link.next, Some(child.as_gc()));
        child.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(!mutator.heap().contains(child_gc));
    let moved_child = unsafe { live_mid_gc.as_non_null().as_ref() }
        .next
        .get()
        .expect("moved child");
    assert!(mutator.heap().contains(moved_child));
    assert_eq!(
        mutator.heap().space_of(moved_child),
        Some(SpaceKind::Nursery)
    );
    assert_eq!(mutator.heap().remembered_edge_count(), 1);
    assert!(mutator.heap().barrier_event_count() > 0);
}

#[test]
fn major_collection_clears_dead_weak_target() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let target = mutator.alloc(&mut scope, Leaf(200)).expect("alloc target");
    let holder = mutator
        .alloc(
            &mut scope,
            WeakHolder {
                label: 201,
                strong: EdgeCell::default(),
                weak: WeakCell::new(Weak::new(target.as_gc())),
            },
        )
        .expect("alloc holder");
    let target_gc = target.as_gc();
    let holder_gc = holder.as_gc();
    drop(scope);

    let mut keep_scope = mutator.handle_scope();
    let holder = mutator.root(&mut keep_scope, holder_gc);
    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert!(!mutator.heap().contains(target_gc));
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .weak
            .target(),
        None
    );
    assert_eq!(unsafe { holder.as_gc().as_non_null().as_ref() }.label, 201);
}

#[test]
fn major_collection_keeps_live_weak_target() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let target = mutator.alloc(&mut scope, Leaf(210)).expect("alloc target");
    let holder = mutator
        .alloc(
            &mut scope,
            WeakHolder {
                label: 211,
                strong: EdgeCell::default(),
                weak: WeakCell::new(Weak::new(target.as_gc())),
            },
        )
        .expect("alloc holder");

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert!(mutator.heap().contains(target.as_gc()));
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .weak
            .target(),
        Some(target.as_gc())
    );
    assert_eq!(unsafe { holder.as_gc().as_non_null().as_ref() }.label, 211);
}

#[test]
fn minor_collection_clears_dead_nursery_weak_target() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut holder_scope = mutator.handle_scope();
    let holder = mutator
        .alloc(
            &mut holder_scope,
            WeakHolder {
                label: 220,
                strong: EdgeCell::default(),
                weak: WeakCell::default(),
            },
        )
        .expect("alloc holder");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(holder.as_gc()),
        Some(SpaceKind::Nursery)
    );
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(holder.as_gc()),
        Some(SpaceKind::Old)
    );

    {
        let mut target_scope = mutator.handle_scope();
        let target = mutator
            .alloc(&mut target_scope, Leaf(221))
            .expect("alloc target");
        let holder = mutator.root(&mut target_scope, holder.as_gc());
        unsafe {
            holder
                .as_gc()
                .as_non_null()
                .as_ref()
                .weak
                .set(Weak::new(target.as_gc()));
        }
    }

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .weak
            .target(),
        None
    );
}

#[test]
fn minor_collection_keeps_old_weak_target_without_marking_it() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut target_scope = mutator.handle_scope();
    let target = mutator
        .alloc(&mut target_scope, Leaf(230))
        .expect("alloc target");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(target.as_gc()),
        Some(SpaceKind::Old)
    );

    let mut holder_scope = mutator.handle_scope();
    let holder = mutator
        .alloc(
            &mut holder_scope,
            WeakHolder {
                label: 231,
                strong: EdgeCell::default(),
                weak: WeakCell::new(Weak::new(target.as_gc())),
            },
        )
        .expect("alloc holder");

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .weak
            .target(),
        Some(target.as_gc())
    );
    assert_eq!(unsafe { holder.as_gc().as_non_null().as_ref() }.label, 231);
}

#[test]
fn major_collection_ephemeron_keeps_value_when_key_is_live() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (holder_gc, key_gc, value_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(300))
            .expect("alloc key");
        let value = mutator
            .alloc(&mut setup_scope, Leaf(301))
            .expect("alloc value");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    label: 302,
                    strong: EdgeCell::default(),
                    pair: Ephemeron::new(Weak::new(key.as_gc()), Weak::new(value.as_gc())),
                },
            )
            .expect("alloc holder");
        (holder.as_gc(), key.as_gc(), value.as_gc())
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);
    let key = mutator.root(&mut keep_scope, key_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert!(mutator.heap().contains(value_gc));
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }.pair.key(),
        Some(key.as_gc())
    );
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .pair
            .value(),
        Some(value_gc)
    );
    assert_eq!(unsafe { holder.as_gc().as_non_null().as_ref() }.label, 302);
}

#[test]
fn major_collection_ephemeron_clears_when_key_is_dead() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (holder_gc, key_gc, value_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(310))
            .expect("alloc key");
        let value = mutator
            .alloc(&mut setup_scope, Leaf(311))
            .expect("alloc value");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    label: 312,
                    strong: EdgeCell::default(),
                    pair: Ephemeron::new(Weak::new(key.as_gc()), Weak::new(value.as_gc())),
                },
            )
            .expect("alloc holder");
        (holder.as_gc(), key.as_gc(), value.as_gc())
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert!(!mutator.heap().contains(key_gc));
    assert!(!mutator.heap().contains(value_gc));
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }.pair.key(),
        None
    );
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .pair
            .value(),
        None
    );
}

#[test]
fn minor_collection_ephemeron_keeps_nursery_value_when_old_key_is_live() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (holder_gc, key_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(320))
            .expect("alloc key");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    label: 321,
                    strong: EdgeCell::default(),
                    pair: Ephemeron::default(),
                },
            )
            .expect("alloc holder");
        (holder.as_gc(), key.as_gc())
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);
    let key = mutator.root(&mut keep_scope, key_gc);
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(holder.as_gc()),
        Some(SpaceKind::Old)
    );
    assert_eq!(mutator.heap().space_of(key.as_gc()), Some(SpaceKind::Old));

    let stale_value = {
        let mut value_scope = mutator.handle_scope();
        let value = mutator
            .alloc(&mut value_scope, Leaf(322))
            .expect("alloc nursery value");
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .pair
            .set(Weak::new(key.as_gc()), Weak::new(value.as_gc()));
        value.as_gc()
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(!mutator.heap().contains(stale_value));
    let live_value = unsafe { holder.as_gc().as_non_null().as_ref() }
        .pair
        .value()
        .expect("retained ephemeron value");
    assert!(mutator.heap().contains(live_value));
    assert_eq!(
        mutator.heap().space_of(live_value),
        Some(SpaceKind::Nursery)
    );
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }.pair.key(),
        Some(key.as_gc())
    );
}

#[test]
fn minor_collection_ephemeron_clears_when_nursery_key_is_dead() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let holder_gc = {
        let mut setup_scope = mutator.handle_scope();
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    label: 330,
                    strong: EdgeCell::default(),
                    pair: Ephemeron::default(),
                },
            )
            .expect("alloc holder");
        holder.as_gc()
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(
        mutator.heap().space_of(holder.as_gc()),
        Some(SpaceKind::Old)
    );

    let (key_gc, value_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(331))
            .expect("alloc key");
        let value = mutator
            .alloc(&mut setup_scope, Leaf(332))
            .expect("alloc value");
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .pair
            .set(Weak::new(key.as_gc()), Weak::new(value.as_gc()));
        (key.as_gc(), value.as_gc())
    };

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect");
    assert_eq!(cycle.minor_collections, 1);
    assert!(!mutator.heap().contains(key_gc));
    assert!(!mutator.heap().contains(value_gc));
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }.pair.key(),
        None
    );
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .pair
            .value(),
        None
    );
}

#[test]
fn background_collector_can_drive_collector_runtime_surface() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        for byte in 0..16u8 {
            mutator
                .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }
    }

    let mut runtime = heap.collector_runtime();
    let mut collector = BackgroundCollector::default();

    let cycle = collector
        .run_until_idle(&mut runtime)
        .expect("run background collector")
        .expect("finished cycle");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(collector.stats().sessions_started, 1);
    assert_eq!(collector.stats().sessions_finished, 1);
    assert_eq!(runtime.active_major_mark_plan(), None);
    assert_eq!(
        runtime.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
}

#[test]
fn background_service_owns_collector_runtime_loop() {
    let mut heap = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        for byte in 0..16u8 {
            mutator
                .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }
    }

    let mut service = heap.background_service(BackgroundCollectorConfig::default());
    let cycle = service
        .run_until_idle()
        .expect("run background service")
        .expect("finished cycle");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(service.stats().sessions_started, 1);
    assert_eq!(service.stats().sessions_finished, 1);
    assert_eq!(service.active_major_mark_plan(), None);
    assert_eq!(
        service.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
}

#[test]
fn background_worker_owns_autonomous_service_loop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    {
        let mut heap = shared.lock().expect("lock shared heap for allocation");
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        for byte in 0..16u8 {
            mutator
                .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }
    }

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::ZERO,
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if shared
            .last_completed_plan()
            .expect("inspect worker result snapshot")
            .map(|plan| plan.kind)
            == Some(CollectionKind::Major)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background worker did not finish a major cycle before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let stats = worker.join().expect("join background worker");
    assert!(stats.loops >= 1);
    assert_eq!(stats.collector.sessions_started, 1);
    assert_eq!(stats.collector.sessions_finished, 1);

    assert_eq!(
        shared
            .active_major_mark_plan()
            .expect("inspect active major-mark plan"),
        None
    );
    assert_eq!(
        shared
            .last_completed_plan()
            .expect("inspect last completed plan")
            .map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
}

#[test]
fn shared_heap_with_mutator_runs_mutator_closure() {
    let shared = SharedHeap::new(HeapConfig::default());
    let label = shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            let leaf = mutator.alloc(&mut scope, Leaf(7)).expect("alloc leaf");
            unsafe { leaf.as_gc().as_non_null().as_ref() }.0
        })
        .expect("run shared mutator closure");

    assert_eq!(label, 7);
    assert_eq!(
        shared
            .active_major_mark_plan()
            .expect("inspect active plan"),
        None
    );
}

#[test]
fn shared_try_with_mutator_reports_would_block_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_mutator(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    assert_eq!(result, Err(SharedHeapError::WouldBlock));
}

#[test]
fn shared_try_with_heap_read_succeeds_while_heap_is_read_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let _guard = shared.read().expect("read-lock shared heap");

    let nursery_live_bytes = shared
        .try_with_heap_read(|heap| heap.stats().nursery.live_bytes)
        .expect("read heap while another reader is active");

    assert_eq!(nursery_live_bytes, 0);
}

#[test]
fn shared_try_with_mutator_reports_would_block_when_heap_is_read_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let _guard = shared.read().expect("read-lock shared heap");

    let result = shared.try_with_mutator(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    assert_eq!(result, Err(SharedHeapError::WouldBlock));
}

#[test]
fn shared_try_with_mutator_status_returns_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_mutator_status(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    match result {
        Err(SharedHeapAccessError::WouldBlock(status)) => {
            assert_eq!(status.stats.nursery.live_bytes, 0);
            assert!(status.active_major_mark_plan.is_none());
            assert!(status.major_mark_progress.is_none());
        }
        other => panic!("expected snapshot-backed would-block, got {other:?}"),
    }
}

#[test]
fn shared_try_with_mutator_status_reports_active_major_mark_snapshot_when_heap_is_locked() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..32u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_mutator_status(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(11)).expect("alloc leaf");
    });

    match result {
        Err(SharedHeapAccessError::WouldBlock(status)) => {
            assert!(status.active_major_mark_plan.is_some());
            let progress = status
                .major_mark_progress
                .expect("active major-mark progress");
            assert!(!progress.completed);
            assert!(progress.remaining_work > 0);
        }
        other => panic!("expected active-session snapshot-backed would-block, got {other:?}"),
    }
}

#[test]
fn shared_try_with_runtime_status_returns_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_runtime_status(|runtime| runtime.recommended_background_plan());

    match result {
        Err(SharedHeapAccessError::WouldBlock(status)) => {
            assert_eq!(status.stats.nursery.live_bytes, 0);
            assert!(status.recommended_background_plan.is_none());
        }
        other => panic!("expected snapshot-backed runtime would-block, got {other:?}"),
    }
}

#[test]
fn shared_snapshot_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = SharedHeap::new(HeapConfig::default());
    let before = shared.stats().expect("read snapshot stats before lock");

    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            shared
                .stats()
                .expect("read snapshot stats while heap lock held"),
            before
        );
        assert_eq!(
            shared
                .last_completed_plan()
                .expect("read last completed plan while heap lock held"),
            None
        );
        assert_eq!(
            shared
                .active_major_mark_plan()
                .expect("read active plan while heap lock held"),
            None
        );

        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let _leaf = mutator
            .alloc(&mut scope, Leaf(11))
            .expect("alloc leaf under guard");

        assert_eq!(
            shared
                .stats()
                .expect("snapshot remains stable until guard drop"),
            before
        );
    }

    let after = shared
        .stats()
        .expect("read snapshot stats after guard drop");
    assert!(after.nursery.live_bytes > before.nursery.live_bytes);
}

#[test]
fn shared_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let before = shared.status().expect("read shared status before lock");
    assert_eq!(before.recommended_plan.kind, CollectionKind::Minor);
    assert_eq!(before.active_major_mark_plan, None);

    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            shared
                .status()
                .expect("read shared status while heap lock held"),
            before
        );

        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for byte in 0..32u8 {
            mutator
                .alloc(&mut scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf under guard");
        }

        assert_eq!(
            shared
                .status()
                .expect("shared status stays stable until guard drop"),
            before
        );
    }

    let after = shared
        .status()
        .expect("read shared status after guard drop");
    assert!(after.stats.old.live_bytes > before.stats.old.live_bytes);
    assert_eq!(after.recommended_plan.kind, CollectionKind::Major);
    assert_eq!(after.active_major_mark_plan, None);
}

#[test]
fn shared_status_supports_parallel_snapshot_readers() {
    let shared = SharedHeap::new(HeapConfig::default());
    let reads = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();

    for _ in 0..4 {
        let shared = shared.clone();
        let reads = Arc::clone(&reads);
        threads.push(thread::spawn(move || {
            for _ in 0..128 {
                let status = shared.status().expect("read shared status");
                assert_eq!(status.recommended_plan.kind, CollectionKind::Minor);
                reads.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for thread in threads {
        thread.join().expect("join snapshot reader");
    }

    assert_eq!(reads.load(Ordering::Relaxed), 512);
}

#[test]
fn shared_snapshot_major_mark_progress_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();

    let first_progress = shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..32u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }

            let mut plan = mutator.plan_for(CollectionKind::Major);
            plan.worker_count = 1;
            plan.mark_slice_budget = 1;
            mutator.begin_major_mark(plan).expect("begin major mark");
            let _ = mutator
                .poll_active_major_mark()
                .expect("poll first major mark slice");
            mutator
                .major_mark_progress()
                .expect("session major mark progress")
        })
        .expect("seed heap and start major mark");

    let second_progress;
    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            shared
                .major_mark_progress()
                .expect("read snapshot progress while heap lock held"),
            Some(first_progress)
        );

        second_progress = {
            let mut mutator = heap.mutator();
            let _ = mutator
                .poll_active_major_mark()
                .expect("poll second major mark slice");
            mutator
                .major_mark_progress()
                .expect("second session major mark progress")
        };

        assert!(
            second_progress.mark_steps > first_progress.mark_steps
                || second_progress.remaining_work < first_progress.remaining_work
        );
        assert_eq!(
            shared
                .major_mark_progress()
                .expect("snapshot stays stable until guard drop"),
            Some(first_progress)
        );
    }

    assert_eq!(
        shared
            .major_mark_progress()
            .expect("snapshot refreshes after guard drop"),
        Some(second_progress)
    );
}

#[test]
fn shared_snapshot_recommended_background_plan_reads_work_while_heap_lock_is_held_and_refresh_on_drop()
 {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();

    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed heap for background recommendation");

    let before = shared
        .recommended_background_plan()
        .expect("read snapshot recommendation before lock")
        .expect("background recommendation before lock");
    assert_eq!(before.kind, CollectionKind::Major);

    let after;
    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            shared
                .recommended_background_plan()
                .expect("read snapshot recommendation while heap lock held"),
            Some(before.clone())
        );

        let mut runtime = heap.collector_runtime();
        runtime
            .begin_major_mark(before.clone())
            .expect("begin major mark under guard");
        after = heap.recommended_background_plan();

        assert_eq!(
            shared
                .recommended_background_plan()
                .expect("snapshot stays stable until guard drop"),
            Some(before.clone())
        );
    }

    assert_eq!(
        shared
            .recommended_background_plan()
            .expect("snapshot refreshes after guard drop"),
        after
    );
}

#[test]
fn shared_snapshot_recommended_plan_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let before = shared
        .recommended_plan()
        .expect("read snapshot recommended plan before lock");
    assert_eq!(before.kind, CollectionKind::Minor);

    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            shared
                .recommended_plan()
                .expect("read snapshot recommended plan while heap lock held"),
            before
        );

        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for byte in 0..32u8 {
            mutator
                .alloc(&mut scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf under guard");
        }

        assert_eq!(
            shared
                .recommended_plan()
                .expect("snapshot stays stable until guard drop"),
            before
        );
    }

    let after = shared
        .recommended_plan()
        .expect("read snapshot recommended plan after guard drop");
    assert_eq!(after.kind, CollectionKind::Major);
}

#[test]
fn shared_mutator_can_allocate_during_background_worker_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 16,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..128u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed shared heap");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig {
            auto_finish_when_ready: false,
            max_rounds_per_tick: 1,
            ..BackgroundCollectorConfig::default()
        },
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::from_millis(1),
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let active = shared
            .active_major_mark_plan()
            .expect("inspect active major mark plan");
        if active.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background worker did not start a major-mark session before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            let leaf = mutator
                .alloc(&mut scope, OldLeaf([200; 32]))
                .expect("allocate during background session");
            assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 200);
        })
        .expect("allocate through shared mutator while worker active");

    let finish_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let finished = shared
            .with_mutator(|mutator| {
                let _ = mutator
                    .poll_active_major_mark()
                    .expect("advance active major mark from shared mutator");
                mutator
                    .finish_active_major_collection_if_ready()
                    .expect("finish ready major collection from shared mutator")
            })
            .expect("drive collection through shared mutator");
        if finished.is_some() {
            break;
        }

        let completed = shared
            .last_completed_plan()
            .expect("inspect last completed plan")
            .map(|plan| plan.kind);
        if completed == Some(CollectionKind::Major) {
            break;
        }
        assert!(
            Instant::now() < finish_deadline,
            "background worker did not finish a major cycle before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let stats = worker.join().expect("join background worker");
    assert!(stats.collector.sessions_started >= 1);
    assert!(stats.collector.sessions_finished <= stats.collector.sessions_started);
    assert!(stats.collector.ticks > 0);
}

#[test]
fn shared_background_service_drives_shared_heap_without_manual_locking() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed shared heap");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let cycle = service
        .run_until_idle()
        .expect("run shared background service")
        .expect("finished cycle");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(service.stats().sessions_started, 1);
    assert_eq!(service.stats().sessions_finished, 1);
    assert_eq!(
        service
            .heap()
            .last_completed_plan()
            .expect("inspect shared heap plan")
            .map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
    assert_eq!(
        shared
            .recommended_background_plan()
            .expect("inspect shared background plan"),
        None
    );
}

#[test]
fn shared_background_service_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(BackgroundCollectorConfig::default());
    let before = service.status().expect("read service status before lock");
    assert_eq!(before.collector.ticks, 0);
    assert_eq!(before.heap.recommended_plan.kind, CollectionKind::Minor);

    {
        let mut heap = shared.lock().expect("lock shared heap");
        assert_eq!(
            service
                .status()
                .expect("read service status while heap lock held"),
            before
        );

        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for byte in 0..32u8 {
            mutator
                .alloc(&mut scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf under guard");
        }

        assert_eq!(
            service
                .status()
                .expect("service status stays stable until guard drop"),
            before
        );
    }

    let after = service
        .status()
        .expect("read service status after guard drop");
    assert_eq!(after.collector.ticks, before.collector.ticks);
    assert!(after.heap.stats.old.live_bytes > before.heap.stats.old.live_bytes);
    assert_eq!(after.heap.recommended_plan.kind, CollectionKind::Major);
}

#[test]
fn shared_background_service_tick_returns_idle_from_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.tick();

    assert_eq!(result, Ok(BackgroundCollectionStatus::Idle));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn shared_background_service_try_tick_returns_idle_from_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_tick();

    assert_eq!(result, Ok(BackgroundCollectionStatus::Idle));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn shared_background_service_try_run_until_idle_returns_idle_from_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_run_until_idle();

    assert_eq!(result, Ok(None));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn shared_background_service_finish_returns_none_from_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.finish_active_major_collection_if_ready();

    assert_eq!(result, Ok(None));
}

#[test]
fn shared_background_service_try_finish_returns_none_from_snapshot_when_heap_is_locked() {
    let shared = SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_finish_active_major_collection_if_ready();

    assert_eq!(result, Ok(None));
}

#[test]
fn shared_background_service_finish_returns_none_from_snapshot_for_active_not_ready_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(service.finish_active_major_collection_if_ready(), Ok(None));
}

#[test]
fn shared_background_service_try_finish_returns_none_from_snapshot_for_active_not_ready_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(
        service.try_finish_active_major_collection_if_ready(),
        Ok(None)
    );
}

#[test]
fn shared_background_service_finish_returns_none_from_snapshot_for_completed_active_session_when_heap_is_locked()
 {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
            loop {
                let progress = mutator
                    .poll_active_major_mark()
                    .expect("poll active major mark")
                    .expect("major-mark session should stay active");
                if progress.completed {
                    break;
                }
            }
        })
        .expect("seed completed major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(service.finish_active_major_collection_if_ready(), Ok(None));
}

#[test]
fn shared_background_service_tick_returns_ready_from_snapshot_for_completed_active_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
            loop {
                let progress = mutator
                    .poll_active_major_mark()
                    .expect("poll active major mark")
                    .expect("active major-mark progress");
                if progress.completed {
                    break;
                }
            }
        })
        .expect("complete active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..BackgroundCollectorConfig::default()
    });
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.tick();

    match result {
        Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
}

#[test]
fn shared_background_service_tick_returns_progress_from_snapshot_for_active_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.remaining_work > 0);
            assert_eq!(progress.drained_objects, 0);
        }
        other => panic!("expected progress snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn shared_background_service_tick_returns_ready_from_snapshot_for_completed_active_session_with_auto_finish()
 {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
            loop {
                let progress = mutator
                    .poll_active_major_mark()
                    .expect("poll active major mark")
                    .expect("major-mark session should stay active");
                if progress.completed {
                    break;
                }
            }
        })
        .expect("seed completed major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn shared_background_service_tick_aggregates_multiple_rounds_with_short_lock_windows() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..40u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
            let plan = CollectionPlan {
                mark_slice_budget: 1,
                ..mutator.plan_for(CollectionKind::Major)
            };
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed shared active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });

    match service.tick().expect("shared service tick") {
        BackgroundCollectionStatus::Idle => panic!("session should be active"),
        BackgroundCollectionStatus::Finished(_) => {
            panic!("single shared tick should not finish whole session")
        }
        BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single shared tick should not drain the whole session")
        }
        BackgroundCollectionStatus::Progress(progress) => {
            assert_eq!(progress.drained_objects, 4);
            assert_eq!(progress.mark_steps, 4);
            assert_eq!(progress.mark_rounds, 2);
            assert!(progress.remaining_work > 0);
        }
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 2);
}

#[test]
fn shared_background_service_try_tick_aggregates_multiple_rounds_with_short_lock_windows() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..40u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
            let plan = CollectionPlan {
                mark_slice_budget: 1,
                ..mutator.plan_for(CollectionKind::Major)
            };
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed shared active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });

    match service.try_tick().expect("shared service try_tick") {
        BackgroundCollectionStatus::Idle => panic!("session should be active"),
        BackgroundCollectionStatus::Finished(_) => {
            panic!("single shared try_tick should not finish whole session")
        }
        BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single shared try_tick should not drain the whole session")
        }
        BackgroundCollectionStatus::Progress(progress) => {
            assert_eq!(progress.drained_objects, 4);
            assert_eq!(progress.mark_steps, 4);
            assert_eq!(progress.mark_rounds, 2);
            assert!(progress.remaining_work > 0);
        }
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 2);
}

#[test]
fn shared_background_service_try_tick_returns_progress_from_snapshot_for_active_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.remaining_work > 0);
            assert_eq!(progress.drained_objects, 0);
        }
        other => panic!("expected progress snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn shared_background_service_try_tick_reports_progress_while_heap_is_read_locked() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let (release_tx, waiter) = read_lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap read lock");
    waiter.join().expect("join helper read-lock thread");

    match result {
        Ok(BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.remaining_work > 0);
        }
        other => panic!("expected shared-read progress status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert!(service.stats().rounds > 0);
}

#[test]
fn shared_background_service_tick_starts_active_session_while_heap_is_read_locked() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
        })
        .expect("seed shared heap for background auto-start");

    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..BackgroundCollectorConfig::default()
    });
    let _guard = shared.read().expect("read-lock shared heap");

    let result = service.tick();

    match result {
        Ok(BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.drained_objects > 0);
        }
        Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected shared-read auto-start progress, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().sessions_started, 1);
    assert!(service.stats().rounds > 0);
    assert!(
        shared
            .active_major_mark_plan()
            .expect("read shared active plan after shared-read auto-start")
            .is_some()
    );
}

#[test]
fn shared_background_service_try_tick_returns_ready_from_snapshot_for_completed_active_session() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
            loop {
                let progress = mutator
                    .poll_active_major_mark()
                    .expect("poll active major mark")
                    .expect("major-mark session should stay active");
                if progress.completed {
                    break;
                }
            }
        })
        .expect("seed completed major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..BackgroundCollectorConfig::default()
    });
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_tick();

    match result {
        Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn shared_background_service_try_tick_returns_ready_from_snapshot_for_completed_active_session_with_auto_finish()
 {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
            loop {
                let progress = mutator
                    .poll_active_major_mark()
                    .expect("poll active major mark")
                    .expect("major-mark session should stay active");
                if progress.completed {
                    break;
                }
            }
        })
        .expect("seed completed major-mark session");

    let mut service = shared.background_service(BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn background_worker_uses_snapshot_idle_fast_path_when_locked_heap_has_no_work() {
    let shared = SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::ZERO,
    });

    {
        let _guard = shared.lock().expect("lock shared heap");
        thread::sleep(Duration::from_millis(10));
    }

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.loops > 0);
    assert!(stats.snapshot_idle_loops > 0);
    assert_eq!(stats.contention_loops, 0);
}

#[test]
fn background_worker_wakes_early_on_shared_heap_signal() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig {
            auto_finish_when_ready: false,
            ..BackgroundCollectorConfig::default()
        },
        idle_sleep: Duration::from_millis(250),
        busy_sleep: Duration::ZERO,
    });

    let wait_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let status = worker.status().expect("read worker status before wake");
        if status.worker.wait_loops > 0 {
            break;
        }
        assert!(
            Instant::now() < wait_deadline,
            "background worker did not enter wait state before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed old objects to wake worker");

    let wake_deadline = Instant::now() + Duration::from_millis(150);
    loop {
        let active = shared
            .active_major_mark_plan()
            .expect("inspect active major-mark plan");
        if active.is_some() {
            break;
        }
        assert!(
            Instant::now() < wake_deadline,
            "background worker did not wake on shared-heap signal before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    assert!(start.elapsed() < Duration::from_millis(150));

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.signal_wakeups > 0);
}

#[test]
fn shared_heap_wait_for_change_wakes_on_guard_drop() {
    let shared = SharedHeap::new(HeapConfig::default());
    let observed_epoch = shared.epoch().expect("read initial shared epoch");
    let waking_shared = shared.clone();
    let waiter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        waking_shared
            .with_mutator(|mutator| {
                let mut scope = mutator.handle_scope();
                let _leaf = mutator.alloc(&mut scope, Leaf(7)).expect("alloc wake leaf");
            })
            .expect("mutate shared heap");
    });

    let (next_epoch, changed) = shared
        .wait_for_change(observed_epoch, Duration::from_secs(1))
        .expect("wait for shared epoch change");
    waiter.join().expect("join waking thread");

    assert!(changed);
    assert!(next_epoch > observed_epoch);
    assert!(
        shared
            .status()
            .expect("read status after wake")
            .stats
            .nursery
            .live_bytes
            > 0
    );
}

#[test]
fn shared_heap_wait_for_change_ignores_read_only_guard_drop() {
    let shared = SharedHeap::new(HeapConfig::default());
    let observed_epoch = shared.epoch().expect("read initial shared epoch");
    let waking_shared = shared.clone();
    let waiter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let _guard = waking_shared
            .lock()
            .expect("lock shared heap without mutation");
        thread::sleep(Duration::from_millis(10));
    });

    let (next_epoch, changed) = shared
        .wait_for_change(observed_epoch, Duration::from_millis(80))
        .expect("wait for shared epoch change");
    waiter.join().expect("join read-only thread");

    assert!(!changed);
    assert_eq!(next_epoch, observed_epoch);
}

#[test]
fn shared_background_service_wait_for_change_delegates_to_shared_heap_signal() {
    let shared = SharedHeap::new(HeapConfig::default());
    let service = shared.background_service(BackgroundCollectorConfig::default());
    let observed_epoch = shared.epoch().expect("read initial shared epoch");
    let waking_shared = shared.clone();
    let waiter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        waking_shared
            .with_mutator(|mutator| {
                let mut scope = mutator.handle_scope();
                let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc wake leaf");
            })
            .expect("mutate shared heap");
    });

    let (next_epoch, changed) = service
        .wait_for_change(observed_epoch, Duration::from_secs(1))
        .expect("wait for service-visible shared-heap change");
    waiter.join().expect("join waking thread");

    assert!(changed);
    assert!(next_epoch > observed_epoch);
}

#[test]
fn shared_background_status_matches_shared_heap_status_background_view() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed old objects");
    let mut service = shared.background_service(BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..BackgroundCollectorConfig::default()
    });
    let _ = service.tick().expect("advance shared background service");

    let heap_status = shared.status().expect("read shared heap status");
    let background_status = shared
        .background_status()
        .expect("read shared background status");

    assert_eq!(
        background_status.recommended_background_plan,
        heap_status.recommended_background_plan
    );
    assert_eq!(
        background_status.active_major_mark_plan,
        heap_status.active_major_mark_plan
    );
    assert_eq!(
        background_status.major_mark_progress,
        heap_status.major_mark_progress
    );
}

#[test]
fn heap_shared_snapshot_matches_shared_status_view() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();

    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..8u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed old objects");

    let snapshot = shared
        .with_heap(|heap| heap.stats())
        .expect("capture heap stats snapshot");
    let status = shared.status().expect("read shared status");

    assert_eq!(snapshot, status.stats);
}

#[test]
fn collector_shared_snapshot_matches_shared_background_status_view() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();

    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..8u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed old objects");

    let snapshot = shared
        .with_heap(|heap| heap.collector_shared_snapshot())
        .expect("capture collector shared snapshot");
    let status = shared
        .background_status()
        .expect("read shared background status");

    assert_eq!(
        snapshot.recommended_background_plan,
        status.recommended_background_plan
    );
    assert_eq!(
        snapshot.active_major_mark_plan,
        status.active_major_mark_plan
    );
    assert_eq!(snapshot.major_mark_progress, status.major_mark_progress);
}

#[test]
fn shared_background_observation_stays_stable_under_lock_and_refreshes_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();

    let before = shared
        .background_observation()
        .expect("read shared background observation before lock");
    assert!(before.status.recommended_background_plan.is_none());

    {
        let mut heap = shared.lock().expect("lock shared heap");
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        for byte in 0..8u8 {
            mutator
                .alloc(&mut scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }

        let during = shared
            .background_observation()
            .expect("read shared background observation while heap lock held");
        assert_eq!(during, before);
    }

    let after = shared
        .background_observation()
        .expect("read shared background observation after guard drop");
    assert!(after.epoch > before.epoch);
    assert!(after.status.recommended_background_plan.is_some());
}

#[test]
fn shared_background_service_wait_for_background_change_reports_old_work_change() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(BackgroundCollectorConfig::default());
    let observed_epoch = shared
        .background_epoch()
        .expect("read initial shared background epoch");
    let observed_status = service
        .background_status()
        .expect("read initial shared background status");
    let waking_shared = shared.clone();
    let waiter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        waking_shared
            .with_mutator(|mutator| {
                let mut scope = mutator.handle_scope();
                for byte in 0..16u8 {
                    mutator
                        .alloc(&mut scope, OldLeaf([byte; 32]))
                        .expect("alloc old leaf");
                }
            })
            .expect("mutate shared heap");
    });

    let wake = service
        .wait_for_background_change(observed_epoch, &observed_status, Duration::from_secs(1))
        .expect("wait for shared background-state change");
    waiter.join().expect("join waking thread");

    assert!(wake.signal_changed);
    assert!(wake.background_changed);
    assert!(wake.next_epoch > observed_epoch);
    assert_ne!(wake.status, observed_status);
    assert!(wake.status.recommended_background_plan.is_some());
}

#[test]
fn shared_background_service_wait_for_background_change_ignores_nursery_only_mutation() {
    let leaf_bytes = estimated_allocation_size::<Leaf>().expect("leaf allocation size");
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: leaf_bytes,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(BackgroundCollectorConfig::default());
    let observed_epoch = shared
        .background_epoch()
        .expect("read initial shared background epoch");
    let observed_status = service
        .background_status()
        .expect("read initial shared background status");
    let waking_shared = shared.clone();
    let waiter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        waking_shared
            .with_mutator(|mutator| {
                let mut scope = mutator.handle_scope();
                let _leaf = mutator
                    .alloc(&mut scope, Leaf(42))
                    .expect("alloc nursery leaf");
            })
            .expect("mutate shared heap");
    });

    let wake = service
        .wait_for_background_change(observed_epoch, &observed_status, Duration::from_millis(100))
        .expect("wait for shared background-state change");
    waiter.join().expect("join waking thread");

    assert!(!wake.signal_changed);
    assert!(!wake.background_changed);
    assert_eq!(wake.next_epoch, observed_epoch);
    assert_eq!(wake.status, observed_status);
}

#[test]
fn background_worker_request_stop_wakes_waiting_worker() {
    let shared = SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(250),
        busy_sleep: Duration::ZERO,
    });

    let wait_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let status = worker.status().expect("read worker status before stop");
        if status.worker.wait_loops > 0 {
            break;
        }
        assert!(
            Instant::now() < wait_deadline,
            "background worker did not enter wait state before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(start.elapsed() < Duration::from_millis(150));
    assert!(stats.wait_loops > 0);
    assert!(stats.signal_wakeups > 0);
}

#[test]
fn background_worker_new_work_wakes_busy_sleeping_worker() {
    let nursery_payload_limit = core::mem::size_of::<Leaf>();
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: nursery_payload_limit,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc initial old leaf");
            }
        })
        .expect("seed initial old objects");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::from_millis(250),
    });

    let first_cycle_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let status = worker
            .status()
            .expect("read worker status before second wake");
        if status.worker.collector.sessions_finished > 0 {
            break;
        }
        assert!(
            Instant::now() < first_cycle_deadline,
            "background worker did not finish first cycle before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let start = Instant::now();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 16..32u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc second old leaf");
            }
        })
        .expect("seed second old objects");

    let second_cycle_deadline = Instant::now() + Duration::from_millis(150);
    loop {
        let status = worker
            .status()
            .expect("read worker status after second wake");
        if status.worker.collector.sessions_started >= 2 {
            break;
        }
        assert!(
            Instant::now() < second_cycle_deadline,
            "background worker did not wake from busy sleep on fresh work before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    assert!(start.elapsed() < Duration::from_millis(150));

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.collector.sessions_started >= 2);
    assert!(stats.signal_wakeups > 0);
    assert!(stats.background_change_wakeups > 0);
}

#[test]
fn background_worker_nursery_only_mutation_does_not_start_new_background_session() {
    let nursery_payload_limit = core::mem::size_of::<Leaf>();
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: nursery_payload_limit,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("alloc initial old leaf");
            }
        })
        .expect("seed initial old objects");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::from_millis(250),
    });

    let first_cycle_deadline = Instant::now() + Duration::from_secs(1);
    let (baseline_signal_wakeups, baseline_background_change_wakeups) = loop {
        let status = worker
            .status()
            .expect("read worker status before nursery-only mutation");
        if status.worker.collector.sessions_finished > 0 {
            break (
                status.worker.signal_wakeups,
                status.worker.background_change_wakeups,
            );
        }
        assert!(
            Instant::now() < first_cycle_deadline,
            "background worker did not finish first cycle before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    };

    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            let _leaf = mutator
                .alloc(&mut scope, Leaf(123))
                .expect("alloc nursery leaf");
        })
        .expect("seed nursery-only mutation");

    thread::sleep(Duration::from_millis(150));
    let status = worker
        .status()
        .expect("read worker status after nursery-only mutation");
    assert_eq!(status.worker.collector.sessions_started, 1);
    assert_eq!(status.worker.signal_wakeups, baseline_signal_wakeups);
    assert_eq!(
        status.worker.background_change_wakeups,
        baseline_background_change_wakeups
    );

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert_eq!(stats.collector.sessions_started, 1);
    assert_eq!(stats.ignored_signal_wakeups, 0);
}

#[test]
fn background_worker_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::ZERO,
    });
    thread::sleep(Duration::from_millis(10));
    let before = worker.status().expect("read worker status before lock");

    {
        let mut heap = shared.lock().expect("lock shared heap");
        let during = worker
            .status()
            .expect("read worker status while heap lock held");
        assert_eq!(during.heap, before.heap);
        assert!(during.worker.loops >= before.worker.loops);

        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let _leaf = mutator
            .alloc(&mut scope, Leaf(11))
            .expect("alloc leaf under guard");

        let still = worker
            .status()
            .expect("worker status stays stable until guard drop");
        assert_eq!(still.heap, before.heap);
    }

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let after = worker
            .status()
            .expect("read worker status after guard drop");
        if after.heap.stats.nursery.live_bytes > before.heap.stats.nursery.live_bytes {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker status did not observe refreshed heap snapshot before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    worker.request_stop();
    let _ = worker.join().expect("join background worker");
}

#[test]
fn background_worker_records_contention_loops_when_heap_lock_is_held() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            for byte in 0..16u8 {
                mutator
                    .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                    .expect("alloc old leaf");
            }
        })
        .expect("seed shared heap");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::ZERO,
    });

    {
        let _guard = shared.lock().expect("lock shared heap");
        thread::sleep(Duration::from_millis(10));
    }

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let status = worker.status().expect("read worker status");
        if status.worker.contention_loops > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background worker did not record contention before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.contention_loops > 0);
    assert!(stats.wait_loops > 0);
}

#[test]
fn background_worker_does_not_count_active_session_contention_as_idle() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut scope = mutator.handle_scope();
            for byte in 0..64u8 {
                mutator
                    .alloc(&mut scope, OldLeaf([byte; 32]))
                    .expect("allocate old leaf");
            }
            let plan = mutator.plan_for(CollectionKind::Major);
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig::default(),
        idle_sleep: Duration::from_millis(10),
        busy_sleep: Duration::ZERO,
    });

    let _guard = shared.lock().expect("lock shared heap");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let status = worker.status().expect("read worker status");
        if status.worker.contention_loops > 0 {
            assert_eq!(status.worker.idle_loops, 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background worker did not record active-session contention before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.contention_loops > 0);
    assert_eq!(stats.idle_loops, 0);
}

#[test]
fn background_worker_publishes_one_round_snapshot_between_multi_round_ticks() {
    let shared = Heap::new(HeapConfig {
        nursery: NurseryConfig {
            max_regular_object_bytes: 1,
            ..NurseryConfig::default()
        },
        large: LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..LargeObjectSpaceConfig::default()
        },
        old: crate::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..crate::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    shared
        .with_mutator(|mutator| {
            let mut keep_scope = mutator.handle_scope();
            let tail = mutator
                .alloc(
                    &mut keep_scope,
                    SlowLink {
                        delay: Duration::from_millis(25),
                        next: EdgeCell::new(None),
                    },
                )
                .expect("alloc tail");
            let mid = mutator
                .alloc(
                    &mut keep_scope,
                    SlowLink {
                        delay: Duration::from_millis(25),
                        next: EdgeCell::new(Some(tail.as_gc())),
                    },
                )
                .expect("alloc mid");
            let root = mutator
                .alloc(
                    &mut keep_scope,
                    SlowLink {
                        delay: Duration::from_millis(25),
                        next: EdgeCell::new(None),
                    },
                )
                .expect("alloc root");
            mutator.store_edge(&root, 0, |link| &link.next, Some(mid.as_gc()));
            let mut plan = mutator.plan_for(CollectionKind::Major);
            plan.worker_count = 1;
            plan.mark_slice_budget = 1;
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed active major-mark session");

    let observed_epoch = shared
        .background_epoch()
        .expect("read initial background epoch");
    let observed_status = shared
        .background_status()
        .expect("read initial background status");

    let worker = shared.spawn_background_worker(BackgroundWorkerConfig {
        collector: BackgroundCollectorConfig {
            auto_start_concurrent: false,
            auto_finish_when_ready: false,
            max_rounds_per_tick: 2,
        },
        idle_sleep: Duration::from_millis(1),
        busy_sleep: Duration::ZERO,
    });

    let first_change = shared
        .wait_for_background_change(observed_epoch, &observed_status, Duration::from_secs(1))
        .expect("wait for first background change");
    let progress = first_change
        .status
        .major_mark_progress
        .expect("background change should publish major-mark progress");
    assert_eq!(progress.mark_rounds, 1);
    assert_eq!(progress.mark_steps, 1);
    assert!(!progress.completed);

    worker.request_stop();
    let stats = worker.join().expect("join background worker");
    assert!(stats.collector.rounds >= 1);
}
