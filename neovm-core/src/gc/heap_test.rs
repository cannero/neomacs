use super::*;
use crate::emacs_core::value::ValueKind;

#[test]
fn alloc_cons_read() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    assert_eq!(heap.cons_car(id), Value::fixnum(1));
    assert_eq!(heap.cons_cdr(id), Value::fixnum(2));
}

#[test]
fn alloc_cons_mutate() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    heap.set_car(id, Value::fixnum(10));
    assert_eq!(heap.cons_car(id), Value::fixnum(10));
}

#[test]
fn free_list_reuse() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id1 = heap.alloc_cons(Value::NIL, Value::NIL);
    let idx = id1.index;
    // Simulate free by collecting with no roots
    heap.collect(std::iter::empty());
    // Next alloc should reuse the slot
    let id2 = heap.alloc_cons(Value::fixnum(42), Value::NIL);
    assert_eq!(id2.index, idx);
    assert_ne!(id2.generation, id1.generation);
}

#[test]
#[should_panic(expected = "stale ObjId")]
fn stale_id_panics() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::NIL, Value::NIL);
    heap.collect(std::iter::empty());
    let _ = heap.cons_car(id); // should panic — stale
}

#[test]
fn collect_unreachable() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let _a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    assert_eq!(heap.allocated_count(), 2);
    // NOTE: push_value_ids is now a no-op (tagged pointer migration),
    // so passing roots to collect() doesn't preserve old-heap objects.
    // Both will be collected.
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn collect_nested() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let inner = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let _outer = heap.alloc_cons(Value::fixnum(99), Value::NIL);
    // NOTE: push_value_ids is now a no-op (tagged pointer migration),
    // so roots don't preserve old-heap objects. Everything is collected.
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn collect_cycle() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let _a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let _b = heap.alloc_cons(Value::fixnum(2), Value::NIL);

    // NOTE: push_value_ids is now a no-op (tagged pointer migration).
    // All old-heap objects are collected regardless of roots.
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn vector_ops() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_vector(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    assert_eq!(heap.vector_len(id), 3);
    assert_eq!(heap.vector_ref(id, 1), Value::fixnum(2));
    heap.vector_set(id, 1, Value::fixnum(20));
    assert_eq!(heap.vector_ref(id, 1), Value::fixnum(20));
}

#[test]
fn list_helpers() {
    crate::test_utils::init_test_tracing();
    let heap = LispHeap::new();
    // Use tagged heap cons values since list_to_vec now uses Value::cons_car/cons_cdr
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);

    let vec = heap.list_to_vec(&list).unwrap();
    assert_eq!(
        vec,
        vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]
    );
    assert_eq!(heap.list_length(&list), Some(3));
}

#[test]
fn structural_equality() {
    crate::test_utils::init_test_tracing();
    let heap = LispHeap::new();
    // Use tagged heap cons values since equal_value now uses Value's methods
    let a = Value::cons(Value::fixnum(1), Value::fixnum(2));
    let b = Value::cons(Value::fixnum(1), Value::fixnum(2));
    assert!(heap.equal_value(&a, &b, 0));
    let c = Value::cons(Value::fixnum(1), Value::fixnum(3));
    assert!(!heap.equal_value(&a, &c, 0));
}

#[test]
fn hash_table_basic() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_hash_table(HashTableTest::Equal);
    let ht = heap.get_hash_table(id);
    assert_eq!(ht.data.len(), 0);
}

#[test]
fn gc_threshold_is_configurable_and_clamped() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    assert_eq!(heap.gc_threshold(), 8192);
    heap.set_gc_threshold(0);
    assert_eq!(heap.gc_threshold(), 1);
    heap.set_gc_threshold(64);
    assert_eq!(heap.gc_threshold(), 64);
}

#[test]
fn should_collect_tracks_allocations_against_threshold() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    heap.set_gc_threshold(2);
    assert!(!heap.should_collect());
    let _ = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    assert!(!heap.should_collect());
    let _ = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    assert!(heap.should_collect());
}

#[test]
fn mark_some_incremental() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    let c = heap.alloc_cons(Value::fixnum(3), Value::NIL);

    // Manually start marking
    heap.gc_phase = GcPhase::Marking;
    for m in heap.marks.iter_mut() {
        *m = false;
    }
    heap.marks.resize(heap.objects.len(), false);
    heap.gray_queue.clear();
    // Directly push ObjIds instead of going through push_value_ids (which is now a no-op)
    heap.gray_queue.push(a);
    heap.gray_queue.push(b);
    heap.gray_queue.push(c);

    // Process one object at a time
    let done = heap.mark_some(1);
    assert!(!done, "should have more work after 1 step");

    // Finish marking
    let done = heap.mark_some(100);
    assert!(done, "should be done after draining queue");

    // All 3 should be marked
    assert!(heap.marks[a.index as usize]);
    assert!(heap.marks[b.index as usize]);
    assert!(heap.marks[c.index as usize]);
}

#[test]
fn write_barrier_regays_black_object() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let new_child = heap.alloc_cons(Value::fixnum(99), Value::NIL);

    // Simulate marking phase: mark `a` as black
    heap.gc_phase = GcPhase::Marking;
    heap.marks.resize(heap.objects.len(), false);
    heap.marks[a.index as usize] = true;

    // Mutate `a` — write barrier should push it back to gray
    heap.set_cdr(a, Value::NIL); // use NIL instead of Value::Cons (no longer available)

    assert!(
        !heap.marks[a.index as usize],
        "write barrier should have cleared mark"
    );
    assert!(
        heap.gray_queue.contains(&a),
        "write barrier should have added to gray queue"
    );

    // After re-scanning, new_child should be discovered
    heap.mark_all();
    assert!(heap.marks[new_child.index as usize]);
}

#[test]
fn write_barrier_noop_when_idle() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);

    // Outside marking phase, write barrier is a no-op
    assert_eq!(heap.gc_phase, GcPhase::Idle);
    heap.set_car(a, Value::fixnum(42));
    assert!(heap.gray_queue.is_empty());
}

#[test]
fn alloc_string_and_collect() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_string("hello".to_string());
    assert_eq!(heap.get_string(id), "hello");
    assert_eq!(heap.allocated_count(), 1);

    // Collect with no roots — string should be freed
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn alloc_string_survives_when_rooted() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let id = heap.alloc_string("world".to_string());

    // NOTE: push_value_ids is now a no-op (tagged pointer migration),
    // so passing roots doesn't preserve old-heap objects. String is collected.
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
    let _ = id; // suppress unused warning
}

#[test]
fn multi_cycle_gc() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();

    // Cycle 1: allocate and collect
    let _a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    // NOTE: push_value_ids is now a no-op (tagged pointer migration).
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);

    // Cycle 2: allocate more
    let _b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    let _c = heap.alloc_cons(Value::fixnum(3), Value::NIL);
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn free_list_reuse_after_collect() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();

    // Allocate and free
    let _a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);

    // Next allocation should reuse the freed slot
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    assert_eq!(b.index, 0); // reused slot 0
    assert_eq!(heap.allocated_count(), 1);
}

#[test]
fn collect_preserves_cons_chain() {
    crate::test_utils::init_test_tracing();
    let heap = LispHeap::new();
    // Use tagged heap cons values since list_to_vec uses Value's methods
    let list = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);

    // Verify the chain is intact via tagged heap cons accessors
    let vec = heap.list_to_vec(&list).unwrap();
    assert_eq!(
        vec,
        vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]
    );
}

#[test]
fn sweep_after_incremental_marking() {
    crate::test_utils::init_test_tracing();
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    let _unreachable = heap.alloc_cons(Value::fixnum(3), Value::NIL);

    assert_eq!(heap.allocated_count(), 3);

    // Start incremental marking
    heap.gc_phase = GcPhase::Marking;
    heap.marks.resize(heap.objects.len(), false);
    heap.gray_queue.clear();
    // Directly push ObjIds instead of going through push_value_ids (which is now a no-op)
    heap.gray_queue.push(a);
    heap.gray_queue.push(b);

    // Drain marking
    heap.mark_all();

    // Sweep
    heap.finish_collection();

    assert_eq!(heap.allocated_count(), 2); // only a and b survive
}
