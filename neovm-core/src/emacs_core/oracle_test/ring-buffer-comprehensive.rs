//! Comprehensive oracle parity tests for Emacs ring buffer operations:
//! make-ring, ring-p, ring-size, ring-length, ring-empty-p, ring-insert,
//! ring-insert-at-beginning, ring-ref, ring-remove, ring-elements,
//! ring-copy, ring-extend, ring-member. Tests all parameters, edge cases
//! (empty, full, overflow wrap-around).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// make-ring and basic predicates
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_make_ring_predicates() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test make-ring creates a ring, ring-p, ring-size, ring-length, ring-empty-p
    // on a fresh ring and after insertions.
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  (let ((is-ring (ring-p r))
        (size (ring-size r))
        (len0 (ring-length r))
        (empty0 (ring-empty-p r)))
    ;; Insert some elements
    (ring-insert r 'a)
    (ring-insert r 'b)
    (ring-insert r 'c)
    (let ((len3 (ring-length r))
          (empty3 (ring-empty-p r))
          (size3 (ring-size r)))
      ;; ring-p on non-ring objects
      (list is-ring size len0 empty0
            len3 empty3 size3
            (ring-p nil)
            (ring-p 42)
            (ring-p "hello")
            (ring-p '(1 2 3))
            (ring-p (make-ring 1))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-insert and ring-ref
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_insert_and_ref() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-insert adds at the front (newest = index 0).
    // ring-ref 0 is newest, ring-ref (1- length) is oldest.
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  (ring-insert r 'first)
  (ring-insert r 'second)
  (ring-insert r 'third)
  (list
   ;; ring-ref 0 is newest
   (ring-ref r 0)
   ;; ring-ref 1 is second newest
   (ring-ref r 1)
   ;; ring-ref 2 is oldest
   (ring-ref r 2)
   ;; Negative index wraps around
   (ring-ref r -1)
   ;; ring-length
   (ring-length r)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-insert-at-beginning
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_insert_at_beginning() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-insert-at-beginning adds at the end (oldest position).
    // Contrast with ring-insert which adds at the front (newest).
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  ;; ring-insert adds newest-first
  (ring-insert r 'a)
  (ring-insert r 'b)
  (ring-insert r 'c)
  (let ((elements-before (ring-elements r)))
    ;; ring-insert-at-beginning adds as oldest
    (ring-insert-at-beginning r 'z)
    (let ((elements-after (ring-elements r))
          (ref0 (ring-ref r 0))
          (ref-last (ring-ref r (1- (ring-length r)))))
      (list elements-before elements-after ref0 ref-last
            (ring-length r)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-remove
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_remove() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-remove removes the element at the given index and returns it.
    // With no index argument, removes the oldest element.
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  (ring-insert r 'a)
  (ring-insert r 'b)
  (ring-insert r 'c)
  (ring-insert r 'd)
  ;; Elements are: d c b a (newest first)
  (let ((before (ring-elements r)))
    ;; Remove index 1 (which is 'c)
    (let ((removed1 (ring-remove r 1)))
      (let ((after1 (ring-elements r)))
        ;; Remove oldest (no index) — that's 'a
        (let ((removed-oldest (ring-remove r)))
          (let ((after2 (ring-elements r)))
            ;; Remove index 0 (newest, which is 'd)
            (let ((removed0 (ring-remove r 0)))
              (list before removed1 after1
                    removed-oldest after2
                    removed0 (ring-elements r)
                    (ring-length r)))))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-elements
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_elements() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-elements returns a list of all elements, newest first.
    // Test with various insertion patterns.
    let form = r#"(require 'ring)
(let ((r (make-ring 4)))
  ;; Empty ring
  (let ((empty-elems (ring-elements r)))
    ;; Partially filled
    (ring-insert r 10)
    (ring-insert r 20)
    (let ((partial-elems (ring-elements r)))
      ;; Full ring
      (ring-insert r 30)
      (ring-insert r 40)
      (let ((full-elems (ring-elements r)))
        ;; Overflow: oldest dropped
        (ring-insert r 50)
        (let ((overflow-elems (ring-elements r)))
          ;; More overflow
          (ring-insert r 60)
          (ring-insert r 70)
          (list empty-elems partial-elems full-elems
                overflow-elems (ring-elements r)
                (ring-length r) (ring-size r)))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-copy
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_copy() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-copy creates an independent copy. Modifications to one don't affect the other.
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  (ring-insert r 'x)
  (ring-insert r 'y)
  (ring-insert r 'z)
  (let ((r2 (ring-copy r)))
    ;; Same content initially
    (let ((orig-elems (ring-elements r))
          (copy-elems (ring-elements r2)))
      ;; Modify original
      (ring-insert r 'w)
      (ring-remove r2)
      ;; They diverge
      (list orig-elems copy-elems
            (ring-elements r) (ring-elements r2)
            (ring-length r) (ring-length r2)
            (ring-size r) (ring-size r2)
            ;; Both are still rings
            (ring-p r) (ring-p r2)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-member
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_member() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-member returns the index of the element if found, nil otherwise.
    // Uses equal for comparison.
    let form = r#"(require 'ring)
(let ((r (make-ring 8)))
  (ring-insert r "apple")
  (ring-insert r "banana")
  (ring-insert r "cherry")
  (ring-insert r "date")
  (list
   ;; Found elements return their index
   (ring-member r "apple")
   (ring-member r "banana")
   (ring-member r "cherry")
   (ring-member r "date")
   ;; Not found returns nil
   (ring-member r "elderberry")
   (ring-member r "fig")
   ;; After overflow, oldest elements are gone
   (ring-insert r "e1")
   (ring-insert r "e2")
   (ring-insert r "e3")
   (ring-insert r "e4")
   ;; Ring is full at 8, insert one more to overflow
   (ring-insert r "overflow")
   ;; "apple" was oldest, should be gone
   (ring-member r "apple")
   (ring-member r "overflow")))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-extend
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_extend() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-extend adds all elements from a list into the ring.
    let form = r#"(require 'ring)
(let ((r (make-ring 10)))
  (ring-insert r 'base)
  ;; Extend with a list
  (ring-extend r '(x y z))
  (let ((after-extend (ring-elements r))
        (len-after (ring-length r)))
    ;; Extend with another list
    (ring-extend r '(1 2 3))
    (list after-extend len-after
          (ring-elements r)
          (ring-length r))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Overflow wrap-around: ring as a fixed-size FIFO
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_overflow_fifo() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Insert more elements than ring size, verifying that oldest are dropped.
    let form = r#"(require 'ring)
(let ((r (make-ring 3))
      (snapshots nil))
  ;; Insert 1..6 into size-3 ring, take snapshot after each
  (let ((i 1))
    (while (<= i 6)
      (ring-insert r i)
      (setq snapshots (cons (list i (ring-elements r) (ring-length r)) snapshots))
      (setq i (1+ i))))
  ;; After inserting 6 elements into size-3 ring, only last 3 remain
  (let ((final-elems (ring-elements r)))
    (list (nreverse snapshots) final-elems
          (ring-empty-p r)
          (= (ring-length r) (ring-size r)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Mixed types in ring
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_mixed_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Rings can hold heterogeneous values.
    let form = r#"(require 'ring)
(let ((r (make-ring 6)))
  (ring-insert r 42)
  (ring-insert r "hello")
  (ring-insert r 'symbol)
  (ring-insert r '(1 2 3))
  (ring-insert r nil)
  (ring-insert r t)
  (let ((elems (ring-elements r)))
    (list elems
          (ring-ref r 0) ;; t (newest)
          (ring-ref r 5) ;; 42 (oldest)
          (ring-member r "hello")
          (ring-member r nil)
          (ring-member r 'symbol)
          (ring-length r))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// ring-ref with modular wrapping on indices
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_ref_modular() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ring-ref wraps around: index can exceed length.
    let form = r#"(require 'ring)
(let ((r (make-ring 5)))
  (ring-insert r 'a)
  (ring-insert r 'b)
  (ring-insert r 'c)
  ;; Length is 3. ring-ref wraps modular.
  (list
   (ring-ref r 0) ;; c (newest)
   (ring-ref r 1) ;; b
   (ring-ref r 2) ;; a (oldest)
   (ring-ref r 3) ;; wraps to c
   (ring-ref r 4) ;; wraps to b
   (ring-ref r 5) ;; wraps to a
   (ring-ref r 6) ;; wraps to c
   (ring-ref r -1) ;; wraps backwards to a
   (ring-ref r -2) ;; wraps backwards to b
   (ring-ref r -3) ;; wraps backwards to c
   ))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Stress: repeated insert-remove cycles
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ring_buffer_comp_stress_cycles() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Repeatedly insert and remove, checking consistency after each cycle.
    let form = r#"(require 'ring)
(let ((r (make-ring 4))
      (results nil))
  ;; Cycle 1: fill completely, drain completely
  (ring-insert r 1)
  (ring-insert r 2)
  (ring-insert r 3)
  (ring-insert r 4)
  (setq results (cons (list 'full (ring-elements r)) results))
  (ring-remove r)
  (ring-remove r)
  (ring-remove r)
  (ring-remove r)
  (setq results (cons (list 'empty (ring-empty-p r) (ring-length r)) results))
  ;; Cycle 2: partial fill, remove specific index, refill
  (ring-insert r 10)
  (ring-insert r 20)
  (ring-insert r 30)
  (let ((removed (ring-remove r 1)))
    (setq results (cons (list 'partial-remove removed (ring-elements r)) results)))
  (ring-insert r 40)
  (ring-insert r 50)
  (setq results (cons (list 'refilled (ring-elements r) (ring-length r)) results))
  ;; Cycle 3: overflow
  (ring-insert r 60)
  (ring-insert r 70)
  (setq results (cons (list 'overflow (ring-elements r)) results))
  (nreverse results))"#;
    assert_oracle_parity(form);
}
