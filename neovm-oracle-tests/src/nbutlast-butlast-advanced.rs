//! Oracle parity tests for advanced `nbutlast`/`butlast` operations:
//! default N, explicit N, edge cases (N >= length), destructive vs
//! non-destructive behavior, sliding window, and queue trim patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// butlast with default (remove last element)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_butlast_default_n() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (butlast '(1 2 3 4 5))
  (butlast '(a b))
  (butlast '(only))
  (butlast nil)
  ;; Verify the original is not modified
  (let ((orig (list 10 20 30 40)))
    (let ((result (butlast orig)))
      (list result orig (length orig)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// butlast with explicit N parameter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_butlast_explicit_n() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (butlast '(1 2 3 4 5) 0)
  (butlast '(1 2 3 4 5) 1)
  (butlast '(1 2 3 4 5) 2)
  (butlast '(1 2 3 4 5) 3)
  (butlast '(1 2 3 4 5) 4))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// butlast when N >= length (returns nil)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_butlast_n_exceeds_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (butlast '(1 2 3) 3)
  (butlast '(1 2 3) 4)
  (butlast '(1 2 3) 100)
  (butlast '(x) 1)
  (butlast '(x) 2)
  (butlast nil 0)
  (butlast nil 5)
  ;; nbutlast with N >= length
  (nbutlast (list 'a 'b) 2)
  (nbutlast (list 'a 'b) 10))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// nbutlast destructive behavior — modifies original list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_destructive_behavior() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // nbutlast destructively truncates the list in place.
    // The original variable still points to the head, which is now shortened.
    let form = r#"(let ((data (list 1 2 3 4 5)))
  (let ((result (nbutlast data)))
    ;; After nbutlast, data IS modified because nbutlast sets
    ;; the cdr of the second-to-last cons to nil
    (list result
          data
          (length data)
          (eq result data)   ;; result shares structure with data
          ;; Another test: nbutlast with N=2
          (let ((data2 (list 'a 'b 'c 'd 'e)))
            (let ((r2 (nbutlast data2 2)))
              (list r2 data2 (eq r2 data2))))
          ;; nbutlast on a copy doesn't affect original
          (let ((orig (list 10 20 30))
                (copy (copy-sequence (list 10 20 30))))
            (nbutlast copy)
            (list (length orig) (length copy))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// butlast vs nbutlast — non-destructive vs destructive comparison
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_butlast_vs_nbutlast_comparison() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((make-test-list (lambda () (list 1 2 3 4 5 6 7))))
  ;; butlast: original unchanged
  (let ((a (funcall make-test-list)))
    (let ((ba (butlast a 3)))
      (let ((a-len-after (length a))
            (ba-result ba))
        ;; nbutlast: original IS changed
        (let ((b (funcall make-test-list)))
          (let ((nb (nbutlast b 3)))
            (list
             ;; butlast results
             ba-result
             a-len-after   ;; still 7
             ;; nbutlast results
             nb
             (length b)    ;; now 4 because b was mutated
             ;; Both produce equal results
             (equal ba-result nb)
             ;; But butlast made a fresh list
             ;; while nbutlast reused the original
             (eq nb b))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: sliding window using butlast
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_sliding_window() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement a sliding window of fixed size over a stream of values.
    // Window maintains the N most recent items. When a new item arrives,
    // the oldest (first) is dropped via cdr and the new one appended.
    // Use butlast to extract sub-windows for analysis.
    let form = r#"(let ((make-window
           (lambda (size)
             (list nil size nil)))  ;; (items max-size snapshots)
          (window-items (lambda (w) (car w)))
          (window-size (lambda (w) (cadr w)))
          (window-add nil)
          (window-full-p
           (lambda (w)
             (>= (length (car w)) (cadr w))))
          (window-average
           (lambda (w)
             (let ((items (car w))
                   (sum 0))
               (dolist (x items)
                 (setq sum (+ sum x)))
               (if items (/ sum (length items)) 0)))))
      ;; window-add: append item, drop oldest if full
      (setq window-add
            (lambda (w item)
              (let ((items (car w))
                    (max-sz (cadr w)))
                (setq items (append items (list item)))
                (when (> (length items) max-sz)
                  (setq items (cdr items)))
                (list items max-sz nil))))
      ;; Run a stream through a window of size 4
      (let ((win (funcall make-window 4))
            (stream '(10 20 30 40 50 60 70))
            (snapshots nil))
        (dolist (val stream)
          (setq win (funcall window-add win val))
          ;; Snapshot: current window, butlast-2 (first N-2 items), average
          (setq snapshots
                (cons (list (copy-sequence (funcall window-items win))
                            (butlast (funcall window-items win) 2)
                            (funcall window-average win)
                            (funcall window-full-p win))
                      snapshots)))
        (nreverse snapshots)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: queue-like trim operation using nbutlast/butlast
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_queue_trim() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // A bounded queue that auto-trims when exceeding capacity.
    // Uses butlast to preview what trimming would look like,
    // then nbutlast to actually trim. Tracks trim history.
    let form = r#"(let ((make-queue
           (lambda (capacity)
             (list nil capacity 0 nil)))  ;; (items capacity trim-count history)
          (q-items (lambda (q) (car q)))
          (q-capacity (lambda (q) (cadr q)))
          (q-trim-count (lambda (q) (nth 2 q)))
          (q-history (lambda (q) (nth 3 q)))
          (q-enqueue nil)
          (q-dequeue nil)
          (q-trim-from-back nil))
      ;; Enqueue: add to end, auto-trim from front if over capacity
      (setq q-enqueue
            (lambda (q item)
              (let ((items (append (car q) (list item)))
                    (cap (cadr q))
                    (tc (nth 2 q))
                    (hist (nth 3 q)))
                (if (> (length items) cap)
                    ;; Remove oldest (front) items to fit
                    (let ((excess (- (length items) cap)))
                      (let ((trimmed (nthcdr excess items)))
                        (list trimmed cap (+ tc excess)
                              (cons (list 'front-trim excess) hist))))
                  (list items cap tc hist)))))
      ;; Dequeue from front
      (setq q-dequeue
            (lambda (q)
              (let ((items (car q)))
                (if items
                    (list (car items)
                          (list (cdr items) (cadr q) (nth 2 q) (nth 3 q)))
                  (list nil q)))))
      ;; Trim N items from back using nbutlast
      (setq q-trim-from-back
            (lambda (q n)
              (let ((items (copy-sequence (car q))))
                (let ((preview (butlast items n))
                      (destructive-copy (copy-sequence (car q))))
                  (nbutlast destructive-copy n)
                  ;; Verify butlast and nbutlast agree
                  (let ((agree (equal preview destructive-copy)))
                    (list (list preview (cadr q) (+ (nth 2 q) n)
                                (cons (list 'back-trim n) (nth 3 q)))
                          agree))))))
      ;; Exercise the queue
      (let ((q (funcall make-queue 5)))
        ;; Fill beyond capacity
        (dolist (item '(a b c d e f g h))
          (setq q (funcall q-enqueue q item)))
        (let ((after-fill (copy-sequence (funcall q-items q)))
              (fill-trims (funcall q-trim-count q)))
          ;; Trim 2 from back
          (let ((trim-result (funcall q-trim-from-back q 2)))
            (let ((trimmed-q (car trim-result))
                  (trim-agree (cadr trim-result)))
              ;; Dequeue from trimmed queue
              (let ((dq-result (funcall q-dequeue trimmed-q)))
                (list after-fill
                      fill-trims
                      (funcall q-items trimmed-q)
                      trim-agree
                      (car dq-result)   ;; dequeued item
                      (funcall q-items (cadr dq-result)) ;; remaining
                      (funcall q-trim-count trimmed-q))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// butlast/nbutlast with nested lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nbutlast_butlast_nested_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((nested '((1 2) (3 4) (5 6) (7 8) (9 10))))
  (list
   ;; butlast on list of lists
   (butlast nested)
   (butlast nested 2)
   (butlast nested 4)
   ;; Nested elements are shared (not deep-copied)
   (let ((bl (butlast nested 3)))
     (eq (car bl) (car nested)))
   ;; nbutlast on a copy of nested
   (let ((copy (copy-sequence nested)))
     (nbutlast copy 2)
     (list copy (length copy)))
   ;; butlast returns fresh spine — mutating it doesn't affect original
   (let ((bl (butlast nested)))
     (setcar bl 'replaced)
     (list bl (car nested)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
