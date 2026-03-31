use super::*;

#[test]
fn alloc_cons_read() {
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    assert_eq!(heap.cons_car(id), Value::fixnum(1));
    assert_eq!(heap.cons_cdr(id), Value::fixnum(2));
}

#[test]
fn alloc_cons_mutate() {
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    heap.set_car(id, Value::fixnum(10));
    assert_eq!(heap.cons_car(id), Value::fixnum(10));
}

#[test]
fn free_list_reuse() {
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
    let mut heap = LispHeap::new();
    let id = heap.alloc_cons(Value::NIL, Value::NIL);
    heap.collect(std::iter::empty());
    let _ = heap.cons_car(id); // should panic — stale
}

#[test]
fn collect_unreachable() {
    let mut heap = LispHeap::new();
    let _a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    assert_eq!(heap.allocated_count(), 2);
    // Only b is a root
    heap.collect([Value::Cons(b)].into_iter());
    assert_eq!(heap.allocated_count(), 1);
    assert_eq!(heap.cons_car(b), Value::fixnum(2));
}

#[test]
fn collect_nested() {
    let mut heap = LispHeap::new();
    let inner = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let outer = heap.alloc_cons(Value::Cons(inner), Value::NIL);
    heap.collect([Value::Cons(outer)].into_iter());
    assert_eq!(heap.allocated_count(), 2);
    // inner is reachable through outer
    assert_eq!(heap.cons_car(inner), Value::fixnum(1));
}

#[test]
fn collect_cycle() {
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::Cons(a));
    heap.set_cdr(a, Value::Cons(b)); // create cycle a <-> b

    // Both reachable from a
    heap.collect([Value::Cons(a)].into_iter());
    assert_eq!(heap.allocated_count(), 2);

    // Remove root — both should be collected
    heap.collect(std::iter::empty());
    assert_eq!(heap.allocated_count(), 0);
}

#[test]
fn vector_ops() {
    let mut heap = LispHeap::new();
    let id = heap.alloc_vector(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    assert_eq!(heap.vector_len(id), 3);
    assert_eq!(heap.vector_ref(id, 1), Value::fixnum(2));
    heap.vector_set(id, 1, Value::fixnum(20));
    assert_eq!(heap.vector_ref(id, 1), Value::fixnum(20));
}

#[test]
fn list_helpers() {
    let mut heap = LispHeap::new();
    let c3 = heap.alloc_cons(Value::fixnum(3), Value::NIL);
    let c2 = heap.alloc_cons(Value::fixnum(2), Value::Cons(c3));
    let c1 = heap.alloc_cons(Value::fixnum(1), Value::Cons(c2));
    let list = Value::Cons(c1);

    let vec = heap.list_to_vec(&list).unwrap();
    assert_eq!(vec, vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    assert_eq!(heap.list_length(&list), Some(3));
}

#[test]
fn structural_equality() {
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    let b = heap.alloc_cons(Value::fixnum(1), Value::fixnum(2));
    assert!(heap.equal_value(&Value::Cons(a), &Value::Cons(b), 0));
    let c = heap.alloc_cons(Value::fixnum(1), Value::fixnum(3));
    assert!(!heap.equal_value(&Value::Cons(a), &Value::Cons(c), 0));
}

#[test]
fn hash_table_basic() {
    let mut heap = LispHeap::new();
    let id = heap.alloc_hash_table(HashTableTest::Equal);
    let ht = heap.get_hash_table(id);
    assert_eq!(ht.data.len(), 0);
}

#[test]
fn gc_threshold_is_configurable_and_clamped() {
    let mut heap = LispHeap::new();
    assert_eq!(heap.gc_threshold(), 8192);
    heap.set_gc_threshold(0);
    assert_eq!(heap.gc_threshold(), 1);
    heap.set_gc_threshold(64);
    assert_eq!(heap.gc_threshold(), 64);
}

#[test]
fn should_collect_tracks_allocations_against_threshold() {
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
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::Cons(a));
    let c = heap.alloc_cons(Value::fixnum(3), Value::Cons(b));

    // Manually start marking
    heap.gc_phase = GcPhase::Marking;
    for m in heap.marks.iter_mut() {
        *m = false;
    }
    heap.marks.resize(heap.objects.len(), false);
    heap.gray_queue.clear();
    LispHeap::push_value_ids(&Value::Cons(c), &mut heap.gray_queue);

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
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let new_child = heap.alloc_cons(Value::fixnum(99), Value::NIL);

    // Simulate marking phase: mark `a` as black
    heap.gc_phase = GcPhase::Marking;
    heap.marks.resize(heap.objects.len(), false);
    heap.marks[a.index as usize] = true;

    // Mutate `a` — write barrier should push it back to gray
    heap.set_cdr(a, Value::Cons(new_child));

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
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);

    // Outside marking phase, write barrier is a no-op
    assert_eq!(heap.gc_phase, GcPhase::Idle);
    heap.set_car(a, Value::fixnum(42));
    assert!(heap.gray_queue.is_empty());
}

#[test]
fn alloc_string_and_collect() {
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
    let mut heap = LispHeap::new();
    let id = heap.alloc_string("world".to_string());
    let root = ValueKind::String;

    heap.collect(std::iter::once(root));
    assert_eq!(heap.allocated_count(), 1);
    assert_eq!(heap.get_string(id), "world");
}

#[test]
fn multi_cycle_gc() {
    let mut heap = LispHeap::new();

    // Cycle 1: allocate and collect
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    heap.collect(std::iter::once(Value::Cons(a)));
    assert_eq!(heap.allocated_count(), 1);

    // Cycle 2: allocate more, drop old root
    let _b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    let c = heap.alloc_cons(Value::fixnum(3), Value::NIL);
    heap.collect(std::iter::once(Value::Cons(c)));
    // Only c survives, a and b are collected
    assert_eq!(heap.allocated_count(), 1);
}

#[test]
fn free_list_reuse_after_collect() {
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
    let mut heap = LispHeap::new();
    let c3 = heap.alloc_cons(Value::fixnum(3), Value::NIL);
    let c2 = heap.alloc_cons(Value::fixnum(2), Value::Cons(c3));
    let c1 = heap.alloc_cons(Value::fixnum(1), Value::Cons(c2));

    // Also allocate an unreachable cons
    let _orphan = heap.alloc_cons(Value::fixnum(99), Value::NIL);

    // Root is c1 — entire chain should survive
    heap.collect(std::iter::once(Value::Cons(c1)));
    assert_eq!(heap.allocated_count(), 3); // c1, c2, c3

    // Verify chain is still intact
    let vec = heap.list_to_vec(&Value::Cons(c1)).unwrap();
    assert_eq!(vec, vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
}

#[test]
fn sweep_after_incremental_marking() {
    let mut heap = LispHeap::new();
    let a = heap.alloc_cons(Value::fixnum(1), Value::NIL);
    let b = heap.alloc_cons(Value::fixnum(2), Value::NIL);
    let _unreachable = heap.alloc_cons(Value::fixnum(3), Value::NIL);

    assert_eq!(heap.allocated_count(), 3);

    // Start incremental marking
    heap.gc_phase = GcPhase::Marking;
    heap.marks.resize(heap.objects.len(), false);
    heap.gray_queue.clear();
    // Root a and b
    LispHeap::push_value_ids(&Value::Cons(a), &mut heap.gray_queue);
    LispHeap::push_value_ids(&Value::Cons(b), &mut heap.gray_queue);

    // Drain marking
    heap.mark_all();

    // Sweep
    heap.finish_collection();

    assert_eq!(heap.allocated_count(), 2); // only a and b survive
}
