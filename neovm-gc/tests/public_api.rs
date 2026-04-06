use neovm_gc::{
    BarrierKind, CollectionKind, CollectionPhase, EdgeCell, Ephemeron, EphemeronVisitor, Heap,
    HeapConfig, MovePolicy, Relocator, Trace, Tracer, TypeFlags, Weak, WeakCell, WeakProcessor,
    estimated_allocation_size,
};
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
    shared: neovm_gc::SharedHeap,
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
    shared: neovm_gc::SharedHeap,
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
struct LargeLeaf([u8; 80]);

unsafe impl Trace for LargeLeaf {
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

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::WEAK
    }
}

#[derive(Debug)]
struct EphemeronHolder {
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

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::WEAK | TypeFlags::EPHEMERON_KEY
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

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::WEAK | TypeFlags::EPHEMERON_KEY
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

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::WEAK
    }
}

static PUBLIC_FINALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct FinalizableLeaf(u64);

unsafe impl Trace for FinalizableLeaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}

    fn relocate(&self, _relocator: &mut dyn Relocator) {}

    fn finalize(&self) {
        PUBLIC_FINALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn type_flags() -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::FINALIZABLE
    }
}

#[test]
fn public_api_keeps_rooted_pinned_object_across_major_gc() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, PinnedLeaf(500))
        .expect("alloc pinned leaf");

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 500);
}

#[test]
fn public_api_alloc_oversize_promote_to_pinned_object_into_pinned_space() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 8,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, OversizePromoteToPinnedLeaf([7; 32]))
        .expect("alloc oversize promotable pinned leaf");

    assert!(mutator.heap().stats().pinned.live_bytes > 0);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 7);
}

#[test]
fn public_api_minor_collection_promotes_promote_to_pinned_object_into_pinned_space() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            promotion_age: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, PromoteToPinnedLeaf(501))
        .expect("alloc promotable pinned leaf");

    assert!(mutator.heap().stats().nursery.live_bytes > 0);
    assert_eq!(mutator.heap().stats().pinned.live_bytes, 0);

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect promotable pinned leaf");

    assert_eq!(cycle.minor_collections, 1);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert!(mutator.heap().stats().pinned.live_bytes > 0);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 501);
}

#[test]
fn public_api_alloc_immortal_object_into_immortal_space() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, ImmortalLeaf(707))
        .expect("alloc immortal leaf");

    assert_eq!(mutator.heap().object_count(), 1);
    assert_eq!(mutator.heap().stats().nursery.live_bytes, 0);
    assert_eq!(mutator.heap().stats().old.live_bytes, 0);
    assert_eq!(mutator.heap().stats().pinned.live_bytes, 0);
    assert_eq!(mutator.heap().stats().large.live_bytes, 0);
    assert!(mutator.heap().stats().immortal.live_bytes > 0);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 707);
}

#[test]
fn public_api_minor_collection_immortal_object_keeps_young_child_alive() {
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

    {
        let mut child_scope = mutator.handle_scope();
        let child = mutator
            .alloc(&mut child_scope, Leaf(808))
            .expect("alloc child leaf");
        mutator.store_edge(&holder, 0, |holder| &holder.child, Some(child.as_gc()));
    }

    let cycle = mutator
        .collect(CollectionKind::Minor)
        .expect("minor collect immortal holder");

    assert_eq!(cycle.minor_collections, 1);
    assert!(mutator.heap().stats().immortal.live_bytes > 0);
    assert_eq!(mutator.heap().remembered_edge_count(), 0);
    let child = unsafe { holder.as_gc().as_non_null().as_ref() }
        .child
        .get()
        .expect("immortal child");
    assert_eq!(unsafe { child.as_non_null().as_ref() }.0, 808);
}

#[test]
fn public_api_minor_plan_uses_configured_parallel_worker_budget() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            parallel_minor_workers: 4,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator.alloc(&mut scope, Leaf(501)).expect("alloc leaf");

    let plan = mutator.plan_for(CollectionKind::Minor);
    assert_eq!(plan.kind, CollectionKind::Minor);
    assert_eq!(plan.worker_count, 4);
    assert!(plan.mark_slice_budget > 0);
}

#[test]
fn public_api_alloc_auto_collects_under_nursery_pressure() {
    let leaf_bytes = estimated_allocation_size::<Leaf>().expect("leaf allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            semispace_bytes: leaf_bytes,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, Leaf(502))
        .expect("alloc first leaf");
    let second = mutator
        .alloc_auto(&mut scope, Leaf(503))
        .expect("alloc second leaf");

    assert_eq!(mutator.heap().stats().collections.minor_collections, 1);
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[CollectionPhase::Evacuate, CollectionPhase::Reclaim]
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 502);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 503);
}

#[test]
fn public_api_alloc_auto_collects_under_pinned_pressure() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, PinnedLeaf(504))
        .expect("alloc first pinned leaf");
    let second = mutator
        .alloc_auto(&mut scope, PinnedLeaf(505))
        .expect("alloc second pinned leaf");

    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
    assert_eq!(
        mutator.heap().last_completed_plan().map(|plan| plan.kind),
        Some(CollectionKind::Major)
    );
    assert_eq!(
        mutator.heap().recent_phase_trace(),
        &[
            CollectionPhase::InitialMark,
            CollectionPhase::Remark,
            CollectionPhase::Reclaim,
        ]
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 504);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 505);
}

#[test]
fn public_api_alloc_auto_collects_under_large_pressure() {
    let large_bytes = estimated_allocation_size::<LargeLeaf>().expect("large allocation size");
    let mut heap = Heap::new(HeapConfig {
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: large_bytes,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, LargeLeaf([4; 80]))
        .expect("alloc first large leaf");
    let second = mutator
        .alloc_auto(&mut scope, LargeLeaf([5; 80]))
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
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 4);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0[0], 5);
}

#[test]
fn public_api_full_collection_evacuates_live_nursery_objects() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            promotion_age: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, Leaf(541))
        .expect("alloc nursery leaf");
    let initial_gc = leaf.as_gc();
    let large = mutator
        .alloc(&mut scope, LargeLeaf([8; 80]))
        .expect("alloc large leaf");

    let cycle = mutator.collect(CollectionKind::Full).expect("full collect");

    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.promoted_bytes > 0);
    assert_ne!(leaf.as_gc(), initial_gc);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 541);
    assert_eq!(unsafe { large.as_gc().as_non_null().as_ref() }.0[0], 8);
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
fn public_api_recommended_plan_prefers_full_for_large_objects() {
    let mut heap = Heap::new(HeapConfig {
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: 64,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, LargeLeaf([1; 80]))
        .expect("alloc large leaf");

    let plan = mutator.recommended_plan();
    assert_eq!(plan.kind, CollectionKind::Full);
    assert_eq!(plan.phase, CollectionPhase::InitialMark);
    assert!(plan.estimated_reclaim_bytes >= mutator.heap().stats().large.live_bytes);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 1);
}

#[test]
fn public_api_execute_major_plan_records_phase_trace() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, OldLeaf([2; 32]))
        .expect("alloc direct-old leaf");

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
        Some(neovm_gc::CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        })
    );
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 2);
}

#[test]
fn public_api_major_plan_can_mark_in_multiple_slices() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        let leaf = mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc direct-old leaf");
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], byte);
    }

    let plan = neovm_gc::CollectionPlan {
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
fn public_api_execute_major_plan_uses_worker_count_to_reduce_mark_rounds() {
    fn run_major_cycle(worker_count: usize) -> neovm_gc::CollectionStats {
        let mut heap = Heap::new(HeapConfig {
            nursery: neovm_gc::spaces::NurseryConfig {
                max_regular_object_bytes: 1,
                ..neovm_gc::spaces::NurseryConfig::default()
            },
            large: neovm_gc::spaces::LargeObjectSpaceConfig {
                threshold_bytes: usize::MAX,
                ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
            },
            old: neovm_gc::spaces::OldGenConfig {
                region_bytes: 512,
                line_bytes: 16,
                concurrent_mark_workers: worker_count,
                mutator_assist_slices: 0,
                ..neovm_gc::spaces::OldGenConfig::default()
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

        let plan = neovm_gc::CollectionPlan {
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
fn public_api_execute_major_plan_traces_on_multiple_threads_when_worker_count_is_high() {
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
fn public_api_execute_minor_plan_traces_on_multiple_threads_when_worker_count_is_high() {
    let seen_threads = Arc::new(Mutex::new(HashSet::new()));
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            parallel_minor_workers: 4,
            ..neovm_gc::spaces::NurseryConfig::default()
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
fn public_api_execute_major_plan_visits_ephemerons_on_multiple_threads_when_worker_count_is_high() {
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
fn public_api_execute_major_plan_processes_weak_edges_on_multiple_threads_when_worker_count_is_high()
 {
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
fn public_api_persistent_major_mark_session_advances_and_finishes() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    for byte in 0..40u8 {
        let leaf = mutator
            .alloc(&mut keep_scope, OldLeaf([byte; 32]))
            .expect("alloc direct-old leaf");
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], byte);
    }

    let plan = neovm_gc::CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    assert_eq!(
        mutator.collect(CollectionKind::Minor),
        Err(neovm_gc::AllocError::CollectionInProgress)
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
        Some(neovm_gc::CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        })
    );
}

#[test]
fn public_api_persistent_major_mark_root_keeps_existing_object() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let leaf_gc = {
        let mut temp_scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut temp_scope, OldLeaf([17; 32]))
            .expect("alloc old leaf");
        leaf.as_gc()
    };

    let plan = neovm_gc::CollectionPlan {
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
fn public_api_persistent_major_mark_barrier_keeps_new_value() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
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
        .expect("barrier-retained target");
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
fn public_api_active_major_mark_plan_is_visible() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    assert_eq!(
        mutator.active_major_mark_plan(),
        Some(neovm_gc::CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan.clone()
        })
    );
    assert_eq!(
        mutator.recommended_plan(),
        neovm_gc::CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan
        }
    );
    assert_eq!(
        mutator.major_mark_progress(),
        Some(neovm_gc::MajorMarkProgress {
            completed: false,
            drained_objects: 0,
            mark_steps: 0,
            mark_rounds: 0,
            remaining_work: 12,
        })
    );
}

#[test]
fn public_api_allocation_during_active_major_mark_advances_assist_progress() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");
    assert_eq!(
        mutator
            .major_mark_progress()
            .expect("initial progress")
            .mark_steps,
        0
    );

    let added = mutator
        .alloc_auto(&mut keep_scope, OldLeaf([99; 32]))
        .expect("alloc during active major mark");
    assert_eq!(unsafe { added.as_gc().as_non_null().as_ref() }.0[0], 99);

    let progress = mutator
        .major_mark_progress()
        .expect("progress after assisted allocation");
    assert!(progress.mark_steps > 0);
    assert!(progress.remaining_work > 0);
    assert_eq!(
        mutator.active_major_mark_plan(),
        Some(neovm_gc::CollectionPlan {
            phase: CollectionPhase::ConcurrentMark,
            ..plan
        })
    );
}

#[test]
fn public_api_alloc_auto_starts_concurrent_major_mark_under_pinned_pressure() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let first = mutator
        .alloc_auto(&mut scope, PinnedLeaf(504))
        .expect("alloc first pinned leaf");
    let second = mutator
        .alloc_auto(&mut scope, PinnedLeaf(505))
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
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0, 504);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0, 505);
}

#[test]
fn public_api_poll_active_major_mark_and_finish_ready_complete_session() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
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
        Some(neovm_gc::CollectionPlan {
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
fn public_api_poll_active_major_mark_uses_configured_worker_round_width() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 4,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
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
fn public_api_background_collection_round_finishes_active_major_session() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
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
            neovm_gc::BackgroundCollectionStatus::Idle => {
                panic!("session should still be active")
            }
            neovm_gc::BackgroundCollectionStatus::Progress(progress) => {
                assert!(progress.mark_steps > 0);
                assert!(progress.mark_rounds > 0);
            }
            neovm_gc::BackgroundCollectionStatus::ReadyToFinish(_) => {
                panic!("direct background service round should finish immediately")
            }
            neovm_gc::BackgroundCollectionStatus::Finished(cycle) => break cycle,
        }
    };

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.active_major_mark_plan(), None);
    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
    assert_eq!(mutator.heap().object_count(), 40);
}

#[test]
fn public_api_pressure_started_concurrent_session_finishes_via_background_service() {
    let pinned_bytes = estimated_allocation_size::<PinnedLeaf>().expect("pinned allocation size");
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: pinned_bytes,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc_auto(&mut scope, PinnedLeaf(504))
        .expect("alloc first pinned leaf");
    mutator
        .alloc_auto(&mut scope, PinnedLeaf(505))
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
            neovm_gc::BackgroundCollectionStatus::Idle => {
                panic!("concurrent session should be active")
            }
            neovm_gc::BackgroundCollectionStatus::Progress(_) => {}
            neovm_gc::BackgroundCollectionStatus::ReadyToFinish(_) => {
                panic!("direct background service round should finish immediately")
            }
            neovm_gc::BackgroundCollectionStatus::Finished(cycle) => break cycle,
        }
    };

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(mutator.active_major_mark_plan(), None);
    assert_eq!(mutator.heap().stats().collections.major_collections, 1);
}

#[test]
fn public_api_background_collector_auto_starts_and_finishes_concurrent_major_plan() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut collector = neovm_gc::BackgroundCollector::default();
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
fn public_api_background_collector_can_disable_auto_start() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut collector = neovm_gc::BackgroundCollector::new(neovm_gc::BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 1,
    });
    assert_eq!(
        collector
            .tick(&mut mutator)
            .expect("tick background collector"),
        neovm_gc::BackgroundCollectionStatus::Idle
    );
    assert_eq!(collector.stats().sessions_started, 0);
    assert_eq!(mutator.active_major_mark_plan(), None);
}

#[test]
fn public_api_background_collector_auto_starts_and_finishes_concurrent_full_plan() {
    let mut heap = Heap::new(HeapConfig {
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    mutator
        .alloc(&mut scope, LargeLeaf([9; 80]))
        .expect("alloc large leaf");

    let mut collector = neovm_gc::BackgroundCollector::default();
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
fn public_api_recommended_background_plan_prefers_major_even_with_live_nursery() {
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_recommended_background_plan_is_none_when_concurrency_is_disabled() {
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_background_collector_prefers_full_even_with_live_nursery() {
    let mut heap = Heap::new(HeapConfig {
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: 64,
            soft_limit_bytes: usize::MAX,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut collector = neovm_gc::BackgroundCollector::default();
    match collector
        .tick(&mut mutator)
        .expect("tick background collector")
    {
        neovm_gc::BackgroundCollectionStatus::Idle => {
            panic!("background collector should auto-start")
        }
        neovm_gc::BackgroundCollectionStatus::Progress(progress) => {
            assert!(progress.mark_steps > 0);
        }
        neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress) => {
            assert!(progress.completed);
        }
        neovm_gc::BackgroundCollectionStatus::Finished(cycle) => {
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
fn public_api_background_collector_tick_aggregates_multiple_rounds() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let plan = neovm_gc::CollectionPlan {
        mark_slice_budget: 1,
        ..mutator.plan_for(CollectionKind::Major)
    };
    mutator
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let mut collector = neovm_gc::BackgroundCollector::new(neovm_gc::BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });
    match collector
        .tick(&mut mutator)
        .expect("tick background collector")
    {
        neovm_gc::BackgroundCollectionStatus::Idle => panic!("session should be active"),
        neovm_gc::BackgroundCollectionStatus::Finished(_) => {
            panic!("single tick should not finish whole session")
        }
        neovm_gc::BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single tick should not drain the whole session")
        }
        neovm_gc::BackgroundCollectionStatus::Progress(progress) => {
            assert_eq!(progress.drained_objects, 4);
            assert_eq!(progress.mark_steps, 4);
            assert_eq!(progress.mark_rounds, 2);
            assert!(progress.remaining_work > 0);
        }
    }
}

#[test]
fn public_api_background_collector_can_leave_ready_session_for_explicit_finish() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    {
        let mut mutator = heap.mutator();
        let mut keep_scope = mutator.handle_scope();
        for byte in 0..8u8 {
            mutator
                .alloc(&mut keep_scope, OldLeaf([byte; 32]))
                .expect("alloc old leaf");
        }
    }

    let mut runtime = heap.collector_runtime();
    let plan = neovm_gc::CollectionPlan {
        mark_slice_budget: usize::MAX,
        ..runtime
            .recommended_background_plan()
            .expect("background plan")
    };
    runtime
        .begin_major_mark(plan.clone())
        .expect("begin persistent major mark");

    let mut collector = neovm_gc::BackgroundCollector::new(neovm_gc::BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: false,
        max_rounds_per_tick: 1,
    });
    let progress = match collector
        .tick(&mut runtime)
        .expect("tick background collector")
    {
        neovm_gc::BackgroundCollectionStatus::Idle => panic!("session should be active"),
        neovm_gc::BackgroundCollectionStatus::Finished(_) => {
            panic!("tick should not auto-finish the ready session")
        }
        neovm_gc::BackgroundCollectionStatus::Progress(_) => {
            panic!("tick should expose a ready-to-finish session")
        }
        neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress) => progress,
    };

    assert!(progress.completed);
    assert_eq!(progress.remaining_work, 0);
    assert_eq!(collector.stats().sessions_finished, 0);
    assert!(runtime.active_major_mark_plan().is_some());

    let cycle = runtime
        .finish_active_major_collection_if_ready()
        .expect("finish ready session")
        .expect("completed cycle");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(runtime.active_major_mark_plan(), None);
}

#[test]
fn public_api_reports_finalized_objects() {
    PUBLIC_FINALIZE_COUNT.store(0, Ordering::SeqCst);

    let mut heap = Heap::new(HeapConfig::default());
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let leaf = mutator
            .alloc(&mut scope, FinalizableLeaf(501))
            .expect("alloc finalizable leaf");
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 501);
    }

    let cycle = heap.collect(CollectionKind::Major).expect("major collect");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.finalized_objects, 1);
    assert_eq!(heap.stats().collections.finalized_objects, 1);
    assert_eq!(PUBLIC_FINALIZE_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn public_api_reports_reclaimed_bytes_on_major_gc() {
    let mut heap = Heap::new(HeapConfig::default());
    {
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let leaf = mutator.alloc(&mut scope, Leaf(540)).expect("alloc leaf");
        assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0, 540);
    }

    let cycle = heap.collect(CollectionKind::Major).expect("major collect");

    assert_eq!(cycle.major_collections, 1);
    assert!(cycle.reclaimed_bytes > 0);
    assert_eq!(
        heap.stats().collections.reclaimed_bytes,
        cycle.reclaimed_bytes
    );
}

#[test]
fn public_api_clears_dead_weak_target_on_major_gc() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let holder_gc = {
        let mut setup_scope = mutator.handle_scope();
        let target = mutator
            .alloc(&mut setup_scope, Leaf(510))
            .expect("alloc target");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                WeakHolder {
                    strong: EdgeCell::default(),
                    weak: WeakCell::new(Weak::new(target.as_gc())),
                },
            )
            .expect("alloc holder");
        holder.as_gc()
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }
            .weak
            .target(),
        None
    );
}

#[test]
fn public_api_ephemeron_keeps_value_when_key_is_live() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (holder_gc, key_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(520))
            .expect("alloc key");
        let value = mutator
            .alloc(&mut setup_scope, Leaf(521))
            .expect("alloc value");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    strong: EdgeCell::default(),
                    pair: Ephemeron::new(Weak::new(key.as_gc()), Weak::new(value.as_gc())),
                },
            )
            .expect("alloc holder");
        (holder.as_gc(), key.as_gc())
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);
    let key = mutator.root(&mut keep_scope, key_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");

    assert_eq!(cycle.major_collections, 1);
    assert_eq!(
        unsafe { holder.as_gc().as_non_null().as_ref() }.pair.key(),
        Some(key.as_gc())
    );
    let value = unsafe { holder.as_gc().as_non_null().as_ref() }
        .pair
        .value()
        .expect("ephemeron retained value");
    assert_eq!(unsafe { value.as_non_null().as_ref() }.0, 521);
}

#[test]
fn public_api_ephemeron_clears_when_key_is_dead() {
    let mut heap = Heap::new(HeapConfig::default());
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let holder_gc = {
        let mut setup_scope = mutator.handle_scope();
        let key = mutator
            .alloc(&mut setup_scope, Leaf(530))
            .expect("alloc key");
        let value = mutator
            .alloc(&mut setup_scope, Leaf(531))
            .expect("alloc value");
        let holder = mutator
            .alloc(
                &mut setup_scope,
                EphemeronHolder {
                    strong: EdgeCell::default(),
                    pair: Ephemeron::new(Weak::new(key.as_gc()), Weak::new(value.as_gc())),
                },
            )
            .expect("alloc holder");
        holder.as_gc()
    };
    let holder = mutator.root(&mut keep_scope, holder_gc);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");

    assert_eq!(cycle.major_collections, 1);
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
fn public_api_exposes_old_region_stats() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 128,
            line_bytes: 16,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let leaf = mutator
        .alloc(&mut scope, OldLeaf([9; 32]))
        .expect("alloc direct-old leaf");

    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].region_index, 0);
    assert_eq!(regions[0].object_count, 1);
    assert!(regions[0].live_bytes > 0);
    assert!(regions[0].occupied_lines > 0);
    assert_eq!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0], 9);
}

#[test]
fn public_api_exposes_major_collection_plan() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 10);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 12);
}

#[test]
fn public_api_major_plan_reports_zero_compaction_bytes_without_old_region_candidates() {
    let mut heap = Heap::new(HeapConfig {
        pinned: neovm_gc::spaces::PinnedSpaceConfig {
            reserved_bytes: usize::MAX,
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut scope = mutator.handle_scope();
    let pinned = mutator
        .alloc(&mut scope, PinnedLeaf(11))
        .expect("alloc pinned leaf");

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.kind, CollectionKind::Major);
    assert_eq!(plan.target_old_regions, 0);
    assert_eq!(plan.estimated_compaction_bytes, 0);
    assert_eq!(unsafe { pinned.as_gc().as_non_null().as_ref() }.0, 11);
}

#[test]
fn public_api_exposes_major_region_candidates() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: estimated_allocation_size::<OldLeaf>()
                .expect("old allocation size")
                .saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
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
        let leaf = mutator.root(&mut keep_scope, gc);
        assert!(unsafe { leaf.as_gc().as_non_null().as_ref() }.0[0] <= 5);
    }

    let candidates = mutator.heap().major_region_candidates();
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].hole_bytes >= candidates[1].hole_bytes);
    assert!(candidates.iter().all(|region| region.hole_bytes > 0));

    let plan = mutator.plan_for(CollectionKind::Major);
    assert_eq!(plan.target_old_regions, 2);
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
}

#[test]
fn public_api_major_region_candidates_prefer_holey_regions_over_tail_only_sparse_regions() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc, fourth_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([90; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([91; 32]))
            .expect("alloc second old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([92; 32]))
            .expect("alloc third old leaf");
        let fourth = mutator
            .alloc(&mut setup_scope, OldLeaf([93; 32]))
            .expect("alloc fourth old leaf");
        (first.as_gc(), third.as_gc(), fourth.as_gc())
    };
    let _first = mutator.root(&mut keep_scope, first_gc);
    let _third = mutator.root(&mut keep_scope, third_gc);
    let _fourth = mutator.root(&mut keep_scope, fourth_gc);
    let tiny = mutator
        .alloc(&mut keep_scope, TinyOldLeaf([94; 8]))
        .expect("alloc tiny tail-only old leaf");
    assert_eq!(unsafe { tiny.as_gc().as_non_null().as_ref() }.0[0], 94);

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
        "tail-only sparse region should remain freer but no longer be a compaction target"
    );
}

#[test]
fn public_api_major_region_candidates_respect_compaction_byte_budget() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            max_compaction_bytes_per_cycle: old_bytes.saturating_mul(3),
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();
    let rooted: Vec<_> = {
        let mut setup_scope = mutator.handle_scope();
        let leaves = [
            mutator
                .alloc(&mut setup_scope, OldLeaf([110; 32]))
                .expect("alloc old leaf 0")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([111; 32]))
                .expect("alloc old leaf 1")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([112; 32]))
                .expect("alloc old leaf 2")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([113; 32]))
                .expect("alloc old leaf 3")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([114; 32]))
                .expect("alloc old leaf 4")
                .as_gc(),
            mutator
                .alloc(&mut setup_scope, OldLeaf([115; 32]))
                .expect("alloc old leaf 5")
                .as_gc(),
        ];
        vec![leaves[0], leaves[2], leaves[3], leaves[5]]
    };
    for gc in rooted {
        let root = mutator.root(&mut keep_scope, gc);
        assert!(unsafe { root.as_gc().as_non_null().as_ref() }.0[0] >= 110);
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
fn public_api_major_region_candidates_prefer_more_reclaim_efficient_regions_under_budget() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            compaction_candidate_limit: 2,
            selective_reclaim_threshold_bytes: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (a_first, a_third, b_first, b_tiny) = {
        let mut setup_scope = mutator.handle_scope();
        let a_first = mutator
            .alloc(&mut setup_scope, OldLeaf([130; 32]))
            .expect("alloc region-a first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([131; 32]))
            .expect("alloc region-a middle old leaf");
        let a_third = mutator
            .alloc(&mut setup_scope, OldLeaf([132; 32]))
            .expect("alloc region-a third old leaf");
        let b_first = mutator
            .alloc(&mut setup_scope, OldLeaf([133; 32]))
            .expect("alloc region-b first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([134; 32]))
            .expect("alloc region-b middle old leaf");
        let b_tiny = mutator
            .alloc(&mut setup_scope, TinyOldLeaf([135; 8]))
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

    assert_eq!(unsafe { a_first.as_gc().as_non_null().as_ref() }.0[0], 130);
    assert_eq!(unsafe { a_third.as_gc().as_non_null().as_ref() }.0[0], 132);
    assert_eq!(unsafe { b_first.as_gc().as_non_null().as_ref() }.0[0], 133);
    assert_eq!(unsafe { b_tiny.as_gc().as_non_null().as_ref() }.0[0], 135);
}

#[test]
fn public_api_reuses_empty_old_region_after_major_gc() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 1,
            line_bytes: 16,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let second_gc = {
        let mut setup_scope = mutator.handle_scope();
        mutator
            .alloc(&mut setup_scope, OldLeaf([4; 32]))
            .expect("alloc first direct-old leaf");
        let second = mutator
            .alloc(&mut setup_scope, OldLeaf([5; 32]))
            .expect("alloc second direct-old leaf");
        second.as_gc()
    };
    let second = mutator.root(&mut keep_scope, second_gc);
    assert_eq!(mutator.heap().old_region_stats().len(), 2);

    let cycle = mutator
        .collect(CollectionKind::Major)
        .expect("major collect");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 0);
    assert_eq!(cycle.reclaimed_regions, 1);

    let third = mutator
        .alloc(&mut keep_scope, OldLeaf([6; 32]))
        .expect("alloc reused direct-old leaf");
    let regions = mutator.heap().old_region_stats();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].object_count, 1);
    assert_eq!(regions[1].object_count, 1);
    assert_eq!(unsafe { second.as_gc().as_non_null().as_ref() }.0[0], 5);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 6);
}

#[test]
fn public_api_major_gc_repacks_old_regions_after_hole() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 1,
            line_bytes: 16,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([7; 32]))
            .expect("alloc first direct-old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([8; 32]))
            .expect("alloc middle direct-old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([9; 32]))
            .expect("alloc third direct-old leaf");
        (first.as_gc(), third.as_gc())
    };
    let first = mutator.root(&mut keep_scope, first_gc);
    let third = mutator.root(&mut keep_scope, third_gc);

    assert_eq!(mutator.heap().old_region_stats().len(), 3);
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
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 7);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 9);
}

#[test]
fn public_api_major_collection_preserves_non_candidate_hole_in_live_old_region() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([40; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([41; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([42; 32]))
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
    assert_eq!(mutator.heap().major_region_candidates().len(), 0);
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 40);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 42);
}

#[test]
fn public_api_major_collection_compacts_selected_live_old_region() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(4),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: 1,
            compaction_candidate_limit: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([50; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([51; 32]))
            .expect("alloc middle old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([52; 32]))
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
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 50);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 52);
}

#[test]
fn public_api_execute_major_plan_honors_exact_selected_old_regions() {
    let old_bytes = estimated_allocation_size::<OldLeaf>().expect("old allocation size");
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: old_bytes.saturating_mul(3),
            line_bytes: 16,
            selective_reclaim_threshold_bytes: 1,
            compaction_candidate_limit: 1,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    });
    let mut mutator = heap.mutator();
    let mut keep_scope = mutator.handle_scope();

    let (first_gc, third_gc, fourth_gc, sixth_gc) = {
        let mut setup_scope = mutator.handle_scope();
        let first = mutator
            .alloc(&mut setup_scope, OldLeaf([70; 32]))
            .expect("alloc first old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([71; 32]))
            .expect("alloc second old leaf");
        let third = mutator
            .alloc(&mut setup_scope, OldLeaf([72; 32]))
            .expect("alloc third old leaf");
        let fourth = mutator
            .alloc(&mut setup_scope, OldLeaf([73; 32]))
            .expect("alloc fourth old leaf");
        mutator
            .alloc(&mut setup_scope, OldLeaf([74; 32]))
            .expect("alloc fifth old leaf");
        let sixth = mutator
            .alloc(&mut setup_scope, OldLeaf([75; 32]))
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
    let manual_plan = neovm_gc::CollectionPlan {
        target_old_regions: 1,
        selected_old_regions: vec![manual_selected],
        estimated_compaction_bytes: before_manual.live_bytes,
        ..planned
    };

    let cycle = mutator
        .execute_plan(manual_plan.clone())
        .expect("execute explicit major plan");
    assert_eq!(cycle.major_collections, 1);
    assert_eq!(cycle.compacted_regions, 1);

    let after_regions = mutator.heap().old_region_stats();
    assert_eq!(after_regions.len(), before_regions.len());
    let after_manual = after_regions
        .iter()
        .find(|region| region.region_index == manual_selected)
        .expect("manual region stats after compaction");
    let after_preserved = after_regions
        .iter()
        .find(|region| region.region_index == preserved_region)
        .expect("preserved region stats after compaction");
    assert!(after_manual.hole_bytes < before_manual.hole_bytes);
    assert!(after_preserved.hole_bytes > 0);
    assert_eq!(
        mutator.heap().last_completed_plan(),
        Some(neovm_gc::CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..manual_plan
        })
    );
    assert_eq!(unsafe { first.as_gc().as_non_null().as_ref() }.0[0], 70);
    assert_eq!(unsafe { third.as_gc().as_non_null().as_ref() }.0[0], 72);
    assert_eq!(unsafe { fourth.as_gc().as_non_null().as_ref() }.0[0], 73);
    assert_eq!(unsafe { sixth.as_gc().as_non_null().as_ref() }.0[0], 75);
}

#[test]
fn public_api_background_collector_can_drive_collector_runtime_surface() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
    let mut collector = neovm_gc::BackgroundCollector::default();

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
fn public_api_background_service_owns_collector_runtime_loop() {
    let mut heap = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = heap.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_background_worker_owns_autonomous_service_loop() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_shared_heap_with_mutator_runs_mutator_closure() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
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
fn public_api_shared_try_with_mutator_reports_would_block_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_mutator(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    assert_eq!(result, Err(neovm_gc::SharedHeapError::WouldBlock));
}

#[test]
fn public_api_shared_try_with_heap_read_succeeds_while_heap_is_read_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let _guard = shared.read().expect("read-lock shared heap");

    let nursery_live_bytes = shared
        .try_with_heap_read(|heap| heap.stats().nursery.live_bytes)
        .expect("read heap while another reader is active");

    assert_eq!(nursery_live_bytes, 0);
}

#[test]
fn public_api_shared_try_with_mutator_reports_would_block_when_heap_is_read_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let _guard = shared.read().expect("read-lock shared heap");

    let result = shared.try_with_mutator(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    assert_eq!(result, Err(neovm_gc::SharedHeapError::WouldBlock));
}

#[test]
fn public_api_shared_try_with_mutator_status_returns_snapshot_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_mutator_status(|mutator| {
        let mut scope = mutator.handle_scope();
        let _leaf = mutator.alloc(&mut scope, Leaf(9)).expect("alloc leaf");
    });

    match result {
        Err(neovm_gc::SharedHeapAccessError::WouldBlock(status)) => {
            assert_eq!(status.stats.nursery.live_bytes, 0);
            assert!(status.active_major_mark_plan.is_none());
            assert!(status.major_mark_progress.is_none());
        }
        other => panic!("expected snapshot-backed would-block, got {other:?}"),
    }
}

#[test]
fn public_api_shared_try_with_mutator_status_reports_active_major_mark_snapshot_when_heap_is_locked()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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
        Err(neovm_gc::SharedHeapAccessError::WouldBlock(status)) => {
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
fn public_api_shared_try_with_runtime_status_returns_snapshot_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = shared.try_with_runtime_status(|runtime| runtime.recommended_background_plan());

    match result {
        Err(neovm_gc::SharedHeapAccessError::WouldBlock(status)) => {
            assert_eq!(status.stats.nursery.live_bytes, 0);
            assert!(status.recommended_background_plan.is_none());
        }
        other => panic!("expected snapshot-backed runtime would-block, got {other:?}"),
    }
}

#[test]
fn public_api_shared_snapshot_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
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
fn public_api_shared_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_shared_status_supports_parallel_snapshot_readers() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
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
fn public_api_shared_snapshot_major_mark_progress_reads_work_while_heap_lock_is_held_and_refresh_on_drop()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_shared_snapshot_recommended_background_plan_reads_work_while_heap_lock_is_held_and_refresh_on_drop()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_shared_snapshot_recommended_plan_reads_work_while_heap_lock_is_held_and_refresh_on_drop()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_shared_mutator_can_allocate_during_background_worker_session() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 16,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig {
            auto_finish_when_ready: false,
            max_rounds_per_tick: 1,
            ..neovm_gc::BackgroundCollectorConfig::default()
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
fn public_api_shared_background_service_drives_shared_heap_without_manual_locking() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_shared_background_service_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_shared_background_service_tick_returns_idle_from_snapshot_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.tick();

    assert_eq!(result, Ok(neovm_gc::BackgroundCollectionStatus::Idle));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn public_api_shared_background_service_try_tick_returns_idle_from_snapshot_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_tick();

    assert_eq!(result, Ok(neovm_gc::BackgroundCollectionStatus::Idle));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn public_api_shared_background_service_try_run_until_idle_returns_idle_from_snapshot_when_heap_is_locked()
 {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_run_until_idle();

    assert_eq!(result, Ok(None));
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn public_api_shared_background_service_finish_returns_none_from_snapshot_when_heap_is_locked() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.finish_active_major_collection_if_ready();

    assert_eq!(result, Ok(None));
}

#[test]
fn public_api_shared_background_service_try_finish_returns_none_from_snapshot_when_heap_is_locked()
{
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_finish_active_major_collection_if_ready();

    assert_eq!(result, Ok(None));
}

#[test]
fn public_api_shared_background_service_finish_returns_none_from_snapshot_for_active_not_ready_session()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(service.finish_active_major_collection_if_ready(), Ok(None));
}

#[test]
fn public_api_shared_background_service_try_finish_returns_none_from_snapshot_for_active_not_ready_session()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(
        service.try_finish_active_major_collection_if_ready(),
        Ok(None)
    );
}

#[test]
fn public_api_shared_background_service_finish_returns_none_from_snapshot_for_completed_active_session_when_heap_is_locked()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let _guard = shared.lock().expect("lock shared heap");

    assert_eq!(service.finish_active_major_collection_if_ready(), Ok(None));
}

#[test]
fn public_api_shared_background_service_tick_returns_ready_from_snapshot_for_completed_active_session()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..neovm_gc::BackgroundCollectorConfig::default()
    });
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.tick();

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
}

#[test]
fn public_api_shared_background_service_tick_returns_progress_from_snapshot_for_active_session() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::Progress(progress)) => {
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
fn public_api_shared_background_service_tick_returns_ready_from_snapshot_for_completed_active_session_with_auto_finish()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn public_api_shared_background_service_tick_aggregates_multiple_rounds_with_short_lock_windows() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
            let plan = neovm_gc::CollectionPlan {
                mark_slice_budget: 1,
                ..mutator.plan_for(CollectionKind::Major)
            };
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed shared active major-mark session");

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });

    match service.tick().expect("shared service tick") {
        neovm_gc::BackgroundCollectionStatus::Idle => panic!("session should be active"),
        neovm_gc::BackgroundCollectionStatus::Finished(_) => {
            panic!("single shared tick should not finish whole session")
        }
        neovm_gc::BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single shared tick should not drain the whole session")
        }
        neovm_gc::BackgroundCollectionStatus::Progress(progress) => {
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
fn public_api_shared_background_service_try_tick_aggregates_multiple_rounds_with_short_lock_windows()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
            let plan = neovm_gc::CollectionPlan {
                mark_slice_budget: 1,
                ..mutator.plan_for(CollectionKind::Major)
            };
            mutator.begin_major_mark(plan).expect("begin major mark");
        })
        .expect("seed shared active major-mark session");

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_start_concurrent: false,
        auto_finish_when_ready: true,
        max_rounds_per_tick: 2,
    });

    match service.try_tick().expect("shared service try_tick") {
        neovm_gc::BackgroundCollectionStatus::Idle => panic!("session should be active"),
        neovm_gc::BackgroundCollectionStatus::Finished(_) => {
            panic!("single shared try_tick should not finish whole session")
        }
        neovm_gc::BackgroundCollectionStatus::ReadyToFinish(_) => {
            panic!("single shared try_tick should not drain the whole session")
        }
        neovm_gc::BackgroundCollectionStatus::Progress(progress) => {
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
fn public_api_shared_background_service_try_tick_returns_progress_from_snapshot_for_active_session()
{
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::Progress(progress)) => {
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
fn public_api_shared_background_service_try_tick_reports_progress_while_heap_is_read_locked() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let (release_tx, waiter) = read_lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap read lock");
    waiter.join().expect("join helper read-lock thread");

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.remaining_work > 0);
        }
        other => panic!("expected shared-read progress status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert!(service.stats().rounds > 0);
}

#[test]
fn public_api_shared_background_service_tick_starts_active_session_while_heap_is_read_locked() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..neovm_gc::BackgroundCollectorConfig::default()
    });
    let _guard = shared.read().expect("read-lock shared heap");

    let result = service.tick();

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::Progress(progress)) => {
            assert!(!progress.completed);
            assert!(progress.drained_objects > 0);
        }
        Ok(neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress)) => {
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
fn public_api_shared_background_service_try_tick_returns_ready_from_snapshot_for_completed_active_session()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..neovm_gc::BackgroundCollectorConfig::default()
    });
    let _guard = shared.lock().expect("lock shared heap");

    let result = service.try_tick();

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
}

#[test]
fn public_api_shared_background_service_try_tick_returns_ready_from_snapshot_for_completed_active_session_with_auto_finish()
 {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 4,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
    let (release_tx, waiter) = lock_shared_heap_on_other_thread(shared.clone());

    let result = service.try_tick();
    release_tx
        .send(())
        .expect("release helper-thread shared heap write lock");
    waiter.join().expect("join helper write-lock thread");

    match result {
        Ok(neovm_gc::BackgroundCollectionStatus::ReadyToFinish(progress)) => {
            assert!(progress.completed);
            assert_eq!(progress.remaining_work, 0);
        }
        other => panic!("expected ready-to-finish snapshot status, got {other:?}"),
    }
    assert_eq!(service.stats().ticks, 1);
    assert_eq!(service.stats().rounds, 1);
}

#[test]
fn public_api_background_worker_uses_snapshot_idle_fast_path_when_locked_heap_has_no_work() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_wakes_early_on_shared_heap_signal() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig {
            auto_finish_when_ready: false,
            ..neovm_gc::BackgroundCollectorConfig::default()
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
fn public_api_shared_heap_wait_for_change_wakes_on_guard_drop() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
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
fn public_api_shared_heap_wait_for_change_ignores_read_only_guard_drop() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
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
fn public_api_shared_background_service_wait_for_change_delegates_to_shared_heap_signal() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_shared_background_status_matches_shared_heap_status_background_view() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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
    let mut service = shared.background_service(neovm_gc::BackgroundCollectorConfig {
        auto_finish_when_ready: false,
        ..neovm_gc::BackgroundCollectorConfig::default()
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
fn public_api_shared_background_observation_stays_stable_under_lock_and_refreshes_on_drop() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            ..neovm_gc::spaces::OldGenConfig::default()
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
fn public_api_shared_background_service_wait_for_background_change_reports_old_work_change() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_shared_background_service_wait_for_background_change_ignores_nursery_only_mutation() {
    let leaf_bytes = neovm_gc::estimated_allocation_size::<Leaf>().expect("leaf allocation size");
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: leaf_bytes,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        ..HeapConfig::default()
    })
    .into_shared();
    let service = shared.background_service(neovm_gc::BackgroundCollectorConfig::default());
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
fn public_api_background_worker_request_stop_wakes_waiting_worker() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_new_work_wakes_busy_sleeping_worker() {
    let nursery_payload_limit = core::mem::size_of::<Leaf>();
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: nursery_payload_limit,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_nursery_only_mutation_does_not_start_new_background_session() {
    let nursery_payload_limit = core::mem::size_of::<Leaf>();
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: nursery_payload_limit,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_status_reads_work_while_heap_lock_is_held_and_refresh_on_drop() {
    let shared = neovm_gc::SharedHeap::new(HeapConfig::default());
    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_records_contention_loops_when_heap_lock_is_held() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_does_not_count_active_session_contention_as_idle() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            concurrent_mark_workers: 2,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig::default(),
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
fn public_api_background_worker_publishes_one_round_snapshot_between_multi_round_ticks() {
    let shared = Heap::new(HeapConfig {
        nursery: neovm_gc::spaces::NurseryConfig {
            max_regular_object_bytes: 1,
            ..neovm_gc::spaces::NurseryConfig::default()
        },
        large: neovm_gc::spaces::LargeObjectSpaceConfig {
            threshold_bytes: usize::MAX,
            ..neovm_gc::spaces::LargeObjectSpaceConfig::default()
        },
        old: neovm_gc::spaces::OldGenConfig {
            region_bytes: 512,
            line_bytes: 16,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 0,
            ..neovm_gc::spaces::OldGenConfig::default()
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

    let worker = shared.spawn_background_worker(neovm_gc::BackgroundWorkerConfig {
        collector: neovm_gc::BackgroundCollectorConfig {
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
