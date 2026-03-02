//! Oracle parity tests for advanced while loop patterns:
//! nested while with accumulators, while with throw/catch exit,
//! while collecting into hash tables, while with dynamic binding changes,
//! while with buffer operations, while with regexp matching,
//! while with condition-case, iterative algorithms (Newton's method,
//! binary search), and while with destructuring.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;
use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Nested while loops with accumulators building a multiplication table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_nested_accumulator_multiplication_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((i 1) (table nil))
  (while (<= i 5)
    (let ((j 1) (row nil))
      (while (<= j 5)
        (setq row (cons (* i j) row))
        (setq j (1+ j)))
      (setq table (cons (nreverse row) table)))
    (setq i (1+ i)))
  (nreverse table))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Nested while with multiple independent accumulators and cross-references
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_nested_cross_accumulators() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Outer loop produces Fibonacci-like seeds; inner loop generates partial sums
    let form = r#"(let ((outer 0)
      (fibs '(1 1 2 3 5 8 13))
      (all-sums nil)
      (grand-total 0))
  (while fibs
    (let ((seed (car fibs))
          (inner 0)
          (partial nil)
          (running 0))
      (while (< inner seed)
        (setq running (+ running inner))
        (setq partial (cons running partial))
        (setq inner (1+ inner)))
      (setq all-sums (cons (list seed (nreverse partial)) all-sums))
      (setq grand-total (+ grand-total running)))
    (setq fibs (cdr fibs)))
  (list :grand-total grand-total
        :results (nreverse all-sums)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with multiple exit conditions via throw/catch
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_throw_catch_exit() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Search for first element matching multiple criteria, using catch/throw
    let form = r#"(let ((data '(3 7 12 5 20 8 15 6 25 9 30)))
  (catch 'found
    (let ((rest data) (idx 0))
      (while rest
        (let ((x (car rest)))
          (when (and (> x 10) (= (% x 5) 0))
            (throw 'found (list :index idx :value x :msg "divisible by 5 and > 10")))
          (when (and (> x 20) (= (% x 3) 0))
            (throw 'found (list :index idx :value x :msg "divisible by 3 and > 20"))))
        (setq rest (cdr rest))
        (setq idx (1+ idx)))
      (list :not-found t))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Nested catch/throw with while for multi-level break
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_nested_catch_throw_break() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((matrix '((1 2 3 4) (5 6 7 8) (9 10 11 12) (13 14 15 16)))
      (result nil))
  (catch 'outer-break
    (let ((rows matrix) (ri 0))
      (while rows
        (catch 'inner-break
          (let ((cols (car rows)) (ci 0))
            (while cols
              (let ((val (car cols)))
                (when (= val 11)
                  (setq result (list :found-at ri ci val))
                  (throw 'outer-break nil))
                (when (> val 6)
                  (setq result (cons (list ri ci val) result))
                  (throw 'inner-break nil)))
              (setq cols (cdr cols))
              (setq ci (1+ ci)))))
        (setq rows (cdr rows))
        (setq ri (1+ ri)))))
  result)"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While collecting into a hash table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_collect_hash_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((words '("apple" "banana" "avocado" "blueberry" "cherry"
                        "apricot" "blackberry" "cantaloupe" "date" "elderberry"
                        "acai" "boysenberry" "coconut"))
      (ht (make-hash-table :test 'equal))
      (rest nil))
  (setq rest words)
  (while rest
    (let* ((word (car rest))
           (key (substring word 0 1))
           (existing (gethash key ht)))
      (puthash key (cons word existing) ht))
    (setq rest (cdr rest)))
  ;; Extract sorted results
  (let ((keys '("a" "b" "c" "d" "e"))
        (result nil))
    (while keys
      (let ((k (car keys)))
        (setq result (cons (list k (sort (copy-sequence (gethash k ht))
                                         #'string<))
                           result)))
      (setq keys (cdr keys)))
    (nreverse result)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While building frequency histogram with hash table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_frequency_histogram() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((data '(1 3 2 1 4 3 2 1 5 3 2 1 4 3 5 2 1))
      (freq (make-hash-table :test 'eq))
      (rest nil)
      (max-count 0)
      (mode nil))
  (setq rest data)
  (while rest
    (let* ((x (car rest))
           (c (1+ (or (gethash x freq) 0))))
      (puthash x c freq)
      (when (> c max-count)
        (setq max-count c)
        (setq mode x)))
    (setq rest (cdr rest)))
  (let ((sorted-keys (sort (copy-sequence '(1 2 3 4 5))
                           (lambda (a b)
                             (> (gethash a freq) (gethash b freq)))))
        (result nil))
    (dolist (k sorted-keys)
      (setq result (cons (cons k (gethash k freq)) result)))
    (list :mode mode :max-count max-count :histogram (nreverse result))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with dynamic binding changes per iteration (let rebinding)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_dynamic_binding_changes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (defvar neovm--test-dyn-depth 0)
  (unwind-protect
      (let ((results nil)
            (items '(a b c d e)))
        (while items
          (let ((neovm--test-dyn-depth (1+ neovm--test-dyn-depth)))
            (setq results
                  (cons (list (car items) neovm--test-dyn-depth)
                        results))
            ;; Nest even deeper for every other item
            (when (= (% neovm--test-dyn-depth 2) 0)
              (let ((neovm--test-dyn-depth (+ neovm--test-dyn-depth 100)))
                (setq results
                      (cons (list 'nested (car items) neovm--test-dyn-depth)
                            results)))))
          (setq items (cdr items)))
        (list :final-depth neovm--test-dyn-depth
              :trace (nreverse results)))
    (makunbound 'neovm--test-dyn-depth)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with buffer insert/delete/search per iteration
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_buffer_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
  (let ((lines '("The quick brown fox" "jumps over" "the lazy dog"
                 "Pack my box with" "five dozen liquor jugs"))
        (rest nil)
        (line-num 1))
    (setq rest lines)
    (while rest
      (insert (format "%03d: %s\n" line-num (car rest)))
      (setq line-num (1+ line-num))
      (setq rest (cdr rest)))
    ;; Now search and collect matching lines
    (goto-char (point-min))
    (let ((matches nil))
      (while (re-search-forward "\\b[Tt]he\\b" nil t)
        (let ((line-start (line-beginning-position))
              (line-end (line-end-position)))
          (setq matches (cons (buffer-substring-no-properties line-start line-end)
                              matches))
          (goto-char line-end)))
      (list :total-lines (1- line-num)
            :buffer-size (buffer-size)
            :matches (nreverse matches)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with buffer deletion and point tracking
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_buffer_delete_and_track() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
  (insert "aXbXcXdXeXfXgXh")
  (goto-char (point-min))
  (let ((deletions 0)
        (positions nil))
    (while (search-forward "X" nil t)
      (setq positions (cons (point) positions))
      (delete-char -1)
      (setq deletions (1+ deletions)))
    (list :deletions deletions
          :positions (nreverse positions)
          :final-text (buffer-string)
          :final-size (buffer-size))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with regexp matching in loop — extract all groups
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_regexp_group_extraction() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
  (insert "name=Alice age=30 city=NYC name=Bob age=25 city=LA name=Eve age=28 city=SF")
  (goto-char (point-min))
  (let ((entries nil)
        (current nil))
    (while (re-search-forward "\\([a-z]+\\)=\\([^ ]+\\)" nil t)
      (let ((key (match-string 1))
            (val (match-string 2)))
        (cond
         ((string= key "name")
          (when current
            (setq entries (cons (nreverse current) entries)))
          (setq current (list (cons key val))))
         (t
          (setq current (cons (cons key val) current))))))
    ;; Don't forget last entry
    (when current
      (setq entries (cons (nreverse current) entries)))
    (nreverse entries)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with condition-case inside — error recovery loop
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_condition_case_recovery() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((operations '((+ 10 5) (/ 20 4) (/ 10 0) (+ 3 7)
                                      (/ 100 0) (* 6 7) (- 50 8) (/ 0 0)
                                      (+ 99 1)))
      (results nil)
      (errors 0)
      (successes 0)
      (rest nil))
  (setq rest operations)
  (while rest
    (let ((op (car rest)))
      (condition-case err
          (let ((val (eval op)))
            (setq results (cons (list :ok op val) results))
            (setq successes (1+ successes)))
        (arith-error
         (setq results (cons (list :error op (cdr err)) results))
         (setq errors (1+ errors)))))
    (setq rest (cdr rest)))
  (list :successes successes
        :errors errors
        :log (nreverse results)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While implementing Newton's method for square root
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_newtons_method_sqrt() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((targets '(2.0 9.0 16.0 25.0 100.0 0.25 0.01))
      (results nil))
  (dolist (n targets)
    (let ((guess (/ n 2.0))
          (iterations 0)
          (tolerance 1e-10))
      (while (and (< iterations 100)
                  (> (abs (- (* guess guess) n)) tolerance))
        (setq guess (/ (+ guess (/ n guess)) 2.0))
        (setq iterations (1+ iterations)))
      (setq results (cons (list :target n
                                :sqrt (/ (round (* guess 1000000.0)) 1000000.0)
                                :iterations iterations
                                :error (/ (round (* (abs (- (* guess guess) n)) 1e12)) 1e12))
                          results))))
  (nreverse results))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While implementing binary search
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_binary_search() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((sorted-vec [2 5 8 12 16 23 38 42 56 72 91 105 200 340 500])
      (targets '(23 1 500 72 100 42 91 -5 340 600))
      (results nil))
  (dolist (target targets)
    (let ((lo 0)
          (hi (1- (length sorted-vec)))
          (found nil)
          (steps 0))
      (while (and (<= lo hi) (not found))
        (let ((mid (/ (+ lo hi) 2)))
          (setq steps (1+ steps))
          (cond
           ((= (aref sorted-vec mid) target)
            (setq found (list :found t :index mid :steps steps)))
           ((< (aref sorted-vec mid) target)
            (setq lo (1+ mid)))
           (t
            (setq hi (1- mid))))))
      (setq results (cons (or found (list :found nil :target target :steps steps))
                          results))))
  (nreverse results))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While implementing iterative GCD (Euclidean algorithm)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_euclidean_gcd() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--test-gcd
    (lambda (a b)
      (let ((x (abs a)) (y (abs b)) (steps 0) (trace nil))
        (while (/= y 0)
          (setq trace (cons (list x y) trace))
          (let ((temp y))
            (setq y (% x y))
            (setq x temp))
          (setq steps (1+ steps)))
        (list :gcd x :steps steps :trace (nreverse trace)))))
  (unwind-protect
      (list
       (funcall 'neovm--test-gcd 48 18)
       (funcall 'neovm--test-gcd 100 75)
       (funcall 'neovm--test-gcd 17 13)
       (funcall 'neovm--test-gcd 0 5)
       (funcall 'neovm--test-gcd 1071 462)
       (funcall 'neovm--test-gcd 270 192)
       ;; Negative inputs
       (funcall 'neovm--test-gcd -48 18)
       (funcall 'neovm--test-gcd 48 -18))
    (fmakunbound 'neovm--test-gcd)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with destructuring on each iteration (manual)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_destructuring_iteration() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((records '((:name "Alice" :age 30 :score 95)
                        (:name "Bob"   :age 25 :score 87)
                        (:name "Carol" :age 35 :score 92)
                        (:name "Dave"  :age 28 :score 78)
                        (:name "Eve"   :age 32 :score 88)))
      (rest nil)
      (total-age 0)
      (total-score 0)
      (count 0)
      (seniors nil)
      (honor-roll nil))
  (setq rest records)
  (while rest
    (let* ((rec (car rest))
           (name (plist-get rec :name))
           (age (plist-get rec :age))
           (score (plist-get rec :score)))
      (setq total-age (+ total-age age))
      (setq total-score (+ total-score score))
      (setq count (1+ count))
      (when (>= age 30)
        (setq seniors (cons name seniors)))
      (when (>= score 90)
        (setq honor-roll (cons name honor-roll))))
    (setq rest (cdr rest)))
  (list :count count
        :avg-age (/ total-age count)
        :avg-score (/ total-score count)
        :seniors (nreverse seniors)
        :honor-roll (nreverse honor-roll)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While implementing iterative Fibonacci with memoization table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_fibonacci_memo_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((memo (make-hash-table :test 'eq))
      (n 20)
      (i 0))
  (puthash 0 0 memo)
  (puthash 1 1 memo)
  (setq i 2)
  (while (<= i n)
    (puthash i (+ (gethash (- i 1) memo) (gethash (- i 2) memo)) memo)
    (setq i (1+ i)))
  ;; Collect all values
  (let ((result nil) (j 0))
    (while (<= j n)
      (setq result (cons (gethash j memo) result))
      (setq j (1+ j)))
    (nreverse result)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While with complex state machine simulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_state_machine() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate a vending machine state machine
    let form = r#"(let ((transitions '((idle   coin   -> has-coin)
                        (has-coin coin   -> has-two)
                        (has-coin select -> vending)
                        (has-two  select -> vending-change)
                        (has-two  coin   -> has-two)
                        (vending  done   -> idle)
                        (vending-change done -> idle)))
      (events '(coin coin select done coin select done coin coin coin select done))
      (state 'idle)
      (rest nil)
      (trace nil)
      (vend-count 0))
  (setq rest events)
  (while rest
    (let ((event (car rest))
          (found nil)
          (trs transitions))
      (while (and trs (not found))
        (let ((tr (car trs)))
          (when (and (eq (nth 0 tr) state)
                     (eq (nth 1 tr) event))
            (let ((new-state (nth 3 tr)))
              (setq trace (cons (list state event '-> new-state) trace))
              (when (memq new-state '(vending vending-change))
                (setq vend-count (1+ vend-count)))
              (setq state new-state)
              (setq found t))))
        (setq trs (cdr trs)))
      (unless found
        (setq trace (cons (list state event '-> 'invalid) trace))))
    (setq rest (cdr rest)))
  (list :final-state state
        :vend-count vend-count
        :trace-length (length trace)
        :last-3 (let ((t3 nil) (i 0) (rev (nreverse trace)))
                  (while (and rev (< i 3))
                    (setq t3 (cons (car rev) t3))
                    (setq rev (cdr rev))
                    (setq i (1+ i)))
                  (nreverse t3))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// While implementing insertion sort
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_insertion_sort() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((data '(38 27 43 3 9 82 10 64 51 17))
      (vec (vconcat data))
      (i 1)
      (comparisons 0)
      (swaps 0))
  (while (< i (length vec))
    (let ((key (aref vec i))
          (j (1- i)))
      (while (and (>= j 0) (> (aref vec j) key))
        (setq comparisons (1+ comparisons))
        (aset vec (1+ j) (aref vec j))
        (setq swaps (1+ swaps))
        (setq j (1- j)))
      (when (>= j 0)
        (setq comparisons (1+ comparisons)))
      (aset vec (1+ j) key))
    (setq i (1+ i)))
  (list :sorted (append vec nil)
        :comparisons comparisons
        :swaps swaps))"#;
    assert_oracle_parity(form);
}
