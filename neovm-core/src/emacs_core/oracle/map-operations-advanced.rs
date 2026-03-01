//! Advanced oracle parity tests for map operations:
//! mapcar with multiple sequences, mapc return value semantics,
//! mapconcat with separator variations, maphash patterns,
//! mapcan (destructive mapcar+nconc), map with index tracking,
//! map with early termination via catch/throw, and nested maps.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity;

// ---------------------------------------------------------------------------
// mapcan: destructive mapcar + nconc (flatMap)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_mapcan_flatmap() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapcan applies function that returns a list, then nconc's all results
    // Use it as a filter+transform (flatMap) pattern
    let form = r#"(progn
  ;; mapcan to expand each number into its divisors
  (fset 'neovm--map-adv-divisors
    (lambda (n)
      "Return list of proper divisors of N."
      (let ((result nil) (i 1))
        (while (<= i (/ n 2))
          (when (= 0 (% n i))
            (setq result (cons i result)))
          (setq i (1+ i)))
        (nreverse result))))

  (unwind-protect
      (list
        ;; Basic mapcan: flatten lists
        (mapcan #'copy-sequence '((1 2) (3 4) (5 6)))
        ;; mapcan as filter (return nil to exclude)
        (mapcan (lambda (x) (if (> x 3) (list x) nil))
                '(1 2 3 4 5 6 7 8))
        ;; mapcan with expansion: each element -> multiple elements
        (mapcan (lambda (x) (list x (* x 10) (* x 100)))
                '(1 2 3))
        ;; mapcan with divisor expansion
        (mapcan (lambda (n)
                  (let ((divs (funcall 'neovm--map-adv-divisors n)))
                    (if divs (list (cons n divs)) nil)))
                '(2 3 4 5 6 7 8 9 10 11 12))
        ;; mapcan as partition: split evens and odds interleaved
        (let ((evens nil) (odds nil))
          (mapcan (lambda (x)
                    (if (= 0 (% x 2))
                        (progn (setq evens (cons x evens)) nil)
                      (list x)))
                  '(1 2 3 4 5 6 7 8 9 10))
          (list (nreverse evens)))
        ;; mapcan with empty results interspersed
        (mapcan (lambda (x)
                  (cond ((= x 0) nil)
                        ((> x 0) (list (format "+%d" x)))
                        (t (list (format "%d" x)))))
                '(3 0 -1 0 0 2 -5 0 4)))
    (fmakunbound 'neovm--map-adv-divisors)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// maphash patterns: collecting, transforming, and aggregating hash tables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_maphash_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((scores (make-hash-table :test 'equal)))
  ;; Populate: student -> list of scores
  (puthash "alice" '(95 87 92 88) scores)
  (puthash "bob" '(78 82 75 90) scores)
  (puthash "carol" '(100 95 98 97) scores)
  (puthash "dave" '(60 65 70 55) scores)
  (puthash "eve" '(88 90 85 92) scores)

  ;; 1. Collect all keys sorted
  (let ((keys nil))
    (maphash (lambda (k _v) (setq keys (cons k keys))) scores)
    (setq keys (sort keys #'string<))

    ;; 2. Compute averages into new hash table
    (let ((averages (make-hash-table :test 'equal)))
      (maphash (lambda (k v)
                 (puthash k (/ (apply #'+ v) (length v)) averages))
               scores)

      ;; 3. Find max scorer
      (let ((best-name nil) (best-avg 0))
        (maphash (lambda (k v)
                   (when (> v best-avg)
                     (setq best-avg v)
                     (setq best-name k)))
                 averages)

        ;; 4. Count students above threshold (80)
        (let ((above-80 0))
          (maphash (lambda (_k v)
                     (when (>= v 80) (setq above-80 (1+ above-80))))
                   averages)

          ;; 5. Build sorted summary alist
          (let ((summary nil))
            (maphash (lambda (k v)
                       (setq summary (cons (list k v
                                                 (if (>= v 90) "A"
                                                   (if (>= v 80) "B"
                                                     (if (>= v 70) "C" "D"))))
                                     summary)))
                     averages)
            (setq summary (sort summary (lambda (a b) (string< (car a) (car b)))))

            (list keys
                  best-name best-avg
                  above-80
                  summary)))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Map with index tracking: enumerate, zip-with-index, windowed map
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_index_tracking() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  ;; enumerate: mapcar with index via closure
  (fset 'neovm--map-adv-enumerate
    (lambda (lst)
      (let ((idx -1))
        (mapcar (lambda (x)
                  (setq idx (1+ idx))
                  (cons idx x))
                lst))))

  ;; sliding window map: apply fn to consecutive pairs
  (fset 'neovm--map-adv-map-pairs
    (lambda (fn lst)
      (let ((result nil)
            (prev (car lst))
            (rest (cdr lst)))
        (while rest
          (setq result (cons (funcall fn prev (car rest)) result))
          (setq prev (car rest))
          (setq rest (cdr rest)))
        (nreverse result))))

  ;; sliding window of size n
  (fset 'neovm--map-adv-sliding-window
    (lambda (lst n)
      (let ((result nil)
            (len (length lst)))
        (if (<= len n)
            (list (copy-sequence lst))
          (let ((i 0))
            (while (<= (+ i n) len)
              (let ((window nil) (j 0) (sub lst))
                ;; skip to position i
                (let ((skip 0) (tmp lst))
                  (while (< skip i)
                    (setq tmp (cdr tmp))
                    (setq skip (1+ skip)))
                  (setq sub tmp))
                ;; take n elements
                (while (< j n)
                  (setq window (cons (car sub) window))
                  (setq sub (cdr sub))
                  (setq j (1+ j)))
                (setq result (cons (nreverse window) result)))
              (setq i (1+ i))))
          (nreverse result)))))

  (unwind-protect
      (list
        ;; enumerate
        (funcall 'neovm--map-adv-enumerate '(a b c d e))
        ;; enumerate with strings
        (funcall 'neovm--map-adv-enumerate '("hello" "world" "foo"))
        ;; map-pairs: compute deltas
        (funcall 'neovm--map-adv-map-pairs #'- '(10 13 11 17 20 15))
        ;; map-pairs: string concat pairs
        (funcall 'neovm--map-adv-map-pairs
                 (lambda (a b) (concat (symbol-name a) "-" (symbol-name b)))
                 '(alpha beta gamma delta))
        ;; sliding window of size 3
        (funcall 'neovm--map-adv-sliding-window '(1 2 3 4 5 6) 3)
        ;; sliding window moving averages
        (let ((windows (funcall 'neovm--map-adv-sliding-window '(10 20 30 40 50) 3)))
          (mapcar (lambda (w) (/ (apply #'+ w) (length w))) windows))
        ;; combined: enumerate + transform
        (mapcar (lambda (pair)
                  (format "#%d: %s" (car pair) (cdr pair)))
                (funcall 'neovm--map-adv-enumerate '("first" "second" "third"))))
    (fmakunbound 'neovm--map-adv-enumerate)
    (fmakunbound 'neovm--map-adv-map-pairs)
    (fmakunbound 'neovm--map-adv-sliding-window)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Map with early termination via catch/throw
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_early_termination() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  ;; find-first: mapcar-like but stops at first match
  (fset 'neovm--map-adv-find-first
    (lambda (pred lst)
      (catch 'found
        (mapc (lambda (x)
                (when (funcall pred x)
                  (throw 'found x)))
              lst)
        nil)))

  ;; take-while: collect elements while predicate holds
  (fset 'neovm--map-adv-take-while
    (lambda (pred lst)
      (let ((result nil))
        (catch 'done
          (mapc (lambda (x)
                  (if (funcall pred x)
                      (setq result (cons x result))
                    (throw 'done nil)))
                lst))
        (nreverse result))))

  ;; map-until: map until predicate on result is true, return accumulated
  (fset 'neovm--map-adv-map-until
    (lambda (fn pred lst)
      (let ((result nil))
        (catch 'stop
          (mapc (lambda (x)
                  (let ((val (funcall fn x)))
                    (setq result (cons val result))
                    (when (funcall pred val)
                      (throw 'stop nil))))
                lst))
        (nreverse result))))

  ;; reduce-until: fold with early exit when accumulator exceeds limit
  (fset 'neovm--map-adv-reduce-until
    (lambda (fn init lst limit)
      (let ((acc init))
        (catch 'overflow
          (mapc (lambda (x)
                  (setq acc (funcall fn acc x))
                  (when (> acc limit)
                    (throw 'overflow (list 'overflow acc x))))
                lst)
          (list 'ok acc)))))

  (unwind-protect
      (list
        ;; find-first: first even number
        (funcall 'neovm--map-adv-find-first #'evenp '(1 3 5 4 6 8))
        ;; find-first: no match
        (funcall 'neovm--map-adv-find-first #'evenp '(1 3 5 7))
        ;; find-first: first match is first element
        (funcall 'neovm--map-adv-find-first #'evenp '(2 3 4))
        ;; take-while: ascending prefix
        (funcall 'neovm--map-adv-take-while
                 (lambda (x) (< x 5))
                 '(1 2 3 4 5 6 7))
        ;; take-while: all pass
        (funcall 'neovm--map-adv-take-while #'numberp '(1 2 3 4))
        ;; take-while: none pass
        (funcall 'neovm--map-adv-take-while #'stringp '(1 2 3))
        ;; map-until: square numbers until result > 50
        (funcall 'neovm--map-adv-map-until
                 (lambda (x) (* x x))
                 (lambda (v) (> v 50))
                 '(1 2 3 4 5 6 7 8 9 10))
        ;; reduce-until: sum until overflow
        (funcall 'neovm--map-adv-reduce-until #'+ 0 '(10 20 30 40 50) 75)
        ;; reduce-until: no overflow
        (funcall 'neovm--map-adv-reduce-until #'+ 0 '(1 2 3 4 5) 100)
        ;; nested catch/throw: find first list containing a negative
        (catch 'found-neg
          (mapc (lambda (sub)
                  (catch 'inner
                    (mapc (lambda (x)
                            (when (< x 0)
                              (throw 'found-neg (list sub x))))
                          sub)))
                '((1 2 3) (4 5 6) (7 -8 9) (10 11 12)))
          nil))
    (fmakunbound 'neovm--map-adv-find-first)
    (fmakunbound 'neovm--map-adv-take-while)
    (fmakunbound 'neovm--map-adv-map-until)
    (fmakunbound 'neovm--map-adv-reduce-until)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Nested maps: matrix operations, cartesian product, group-by
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_nested_maps() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  ;; Matrix transpose via nested mapcar
  (fset 'neovm--map-adv-transpose
    (lambda (matrix)
      "Transpose a matrix (list of lists)."
      (let ((ncols (length (car matrix)))
            (result nil))
        (let ((col 0))
          (while (< col ncols)
            (setq result
                  (cons (mapcar (lambda (row) (nth col row)) matrix)
                        result))
            (setq col (1+ col))))
        (nreverse result))))

  ;; Matrix multiply via nested maps
  (fset 'neovm--map-adv-mat-mul
    (lambda (a b)
      "Multiply matrices A and B."
      (let ((bt (funcall 'neovm--map-adv-transpose b)))
        (mapcar
         (lambda (row-a)
           (mapcar
            (lambda (col-b)
              (apply #'+ (let ((result nil) (r row-a) (c col-b))
                           (while r
                             (setq result (cons (* (car r) (car c)) result))
                             (setq r (cdr r))
                             (setq c (cdr c)))
                           result)))
            bt))
         a))))

  ;; Group-by via maphash after building hash table
  (fset 'neovm--map-adv-group-by
    (lambda (key-fn lst)
      "Group elements of LST by KEY-FN into an alist."
      (let ((ht (make-hash-table :test 'equal)))
        (mapc (lambda (x)
                (let ((k (funcall key-fn x)))
                  (puthash k (cons x (gethash k ht nil)) ht)))
              lst)
        (let ((result nil))
          (maphash (lambda (k v)
                     (setq result (cons (cons k (nreverse v)) result)))
                   ht)
          (sort result (lambda (a b)
                         (if (numberp (car a))
                             (< (car a) (car b))
                           (string< (format "%s" (car a))
                                    (format "%s" (car b))))))))))

  (unwind-protect
      (list
        ;; transpose
        (funcall 'neovm--map-adv-transpose '((1 2 3) (4 5 6) (7 8 9)))
        ;; transpose non-square
        (funcall 'neovm--map-adv-transpose '((1 2 3 4) (5 6 7 8)))
        ;; matrix multiply identity
        (funcall 'neovm--map-adv-mat-mul
                 '((1 0) (0 1))
                 '((5 6) (7 8)))
        ;; matrix multiply 2x3 * 3x2
        (funcall 'neovm--map-adv-mat-mul
                 '((1 2 3) (4 5 6))
                 '((7 8) (9 10) (11 12)))
        ;; cartesian product via nested mapcan
        (let ((xs '(1 2 3)) (ys '(a b)))
          (mapcan (lambda (x)
                    (mapcar (lambda (y) (list x y)) ys))
                  xs))
        ;; group-by: group numbers by modulo 3
        (funcall 'neovm--map-adv-group-by
                 (lambda (n) (% n 3))
                 '(1 2 3 4 5 6 7 8 9 10 11 12))
        ;; group-by: group strings by length
        (funcall 'neovm--map-adv-group-by
                 #'length
                 '("hi" "cat" "a" "dog" "be" "fish" "go" "ant"))
        ;; nested map pipeline: for each row, compute running sum
        (mapcar (lambda (row)
                  (let ((sum 0))
                    (mapcar (lambda (x)
                              (setq sum (+ sum x))
                              sum)
                            row)))
                '((1 2 3 4) (10 20 30) (5 -3 8 -2 1))))
    (fmakunbound 'neovm--map-adv-transpose)
    (fmakunbound 'neovm--map-adv-mat-mul)
    (fmakunbound 'neovm--map-adv-group-by)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// mapc return value and side-effect accumulation patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_mapc_return_and_accumulate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapc always returns its input list, not collected results
    let form = r#"(let ((input '(1 2 3 4 5)))
  ;; Verify mapc return value is eq to input
  (let ((returned (mapc #'identity input)))
    (let ((same (eq returned input))
          ;; Accumulate multiple side effects simultaneously
          (sum 0)
          (product 1)
          (evens nil)
          (odds nil)
          (running nil))
      (mapc (lambda (x)
              (setq sum (+ sum x))
              (setq product (* product x))
              (if (= 0 (% x 2))
                  (setq evens (cons x evens))
                (setq odds (cons x odds)))
              (setq running (cons sum running)))
            input)
      (list same
            sum product
            (nreverse evens) (nreverse odds)
            (nreverse running)
            ;; mapc on empty list
            (mapc #'identity nil)
            ;; mapc with nested side effects: build adjacency list
            (let ((adj (make-hash-table :test 'equal)))
              (mapc (lambda (edge)
                      (let ((from (car edge)) (to (cdr edge)))
                        (puthash from (cons to (gethash from adj nil)) adj)
                        (puthash to (cons from (gethash to adj nil)) adj)))
                    '(("a" . "b") ("b" . "c") ("a" . "c") ("c" . "d")))
              (let ((result nil))
                (maphash (lambda (k v)
                           (setq result (cons (cons k (sort (copy-sequence v) #'string<))
                                              result)))
                         adj)
                (sort result (lambda (a b) (string< (car a) (car b))))))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// mapconcat: advanced separator and transform patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_mapconcat_complex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; CSV row generation
  (mapconcat (lambda (row)
               (mapconcat (lambda (cell)
                            (cond ((null cell) "")
                                  ((numberp cell) (number-to-string cell))
                                  ((stringp cell)
                                   (if (string-match-p "," cell)
                                       (concat "\"" cell "\"")
                                     cell))
                                  (t (format "%s" cell))))
                          row ","))
             '(("name" "city" "age")
               ("Alice" "New York" 30)
               ("Bob" "San Francisco, CA" 25)
               ("Carol" nil 35))
             "\n")

  ;; Build query string from alist
  (mapconcat (lambda (pair)
               (format "%s=%s"
                       (url-hexify-string (car pair))
                       (url-hexify-string (cdr pair))))
             '(("name" . "Alice Smith") ("age" . "30") ("city" . "NYC"))
             "&")

  ;; Tree pretty-print via recursive mapconcat
  (let ((tree '(root (a (a1) (a2 (a2x))) (b) (c (c1) (c2) (c3)))))
    (letrec ((fmt (lambda (node depth)
                    (let ((name (symbol-name (car node)))
                          (children (cdr node))
                          (prefix (make-string (* depth 2) ?\s)))
                      (if children
                          (concat prefix name "\n"
                                  (mapconcat
                                   (lambda (child) (funcall fmt child (1+ depth)))
                                   children "\n"))
                        (concat prefix name))))))
      (funcall fmt tree 0)))

  ;; Join with alternating separators
  (let ((items '("a" "b" "c" "d" "e"))
        (idx -1))
    (mapconcat (lambda (x)
                 (setq idx (1+ idx))
                 (if (= idx 0)
                     x
                   (concat (if (= 0 (% idx 2)) " + " " - ") x)))
               items ""))

  ;; mapconcat with single-element list
  (mapconcat #'upcase '("only") "---")

  ;; mapconcat on nil
  (mapconcat #'identity nil ","))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Comprehensive map pipeline: ETL (extract, transform, load) simulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_adv_etl_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  ;; Simulate ETL: raw log lines -> parsed records -> aggregated stats
  (fset 'neovm--map-adv-parse-log
    (lambda (line)
      "Parse 'LEVEL:component:message' into record alist."
      (let ((parts (split-string line ":")))
        (when (>= (length parts) 3)
          (list (cons 'level (nth 0 parts))
                (cons 'component (nth 1 parts))
                (cons 'message (mapconcat #'identity (nthcdr 2 parts) ":")))))))

  (fset 'neovm--map-adv-aggregate
    (lambda (records key-fn)
      "Count occurrences grouped by KEY-FN."
      (let ((counts (make-hash-table :test 'equal)))
        (mapc (lambda (r)
                (when r
                  (let ((k (funcall key-fn r)))
                    (puthash k (1+ (gethash k counts 0)) counts))))
              records)
        (let ((result nil))
          (maphash (lambda (k v) (setq result (cons (cons k v) result))) counts)
          (sort result (lambda (a b) (> (cdr a) (cdr b))))))))

  (unwind-protect
      (let ((logs '("ERROR:db:connection timeout"
                    "INFO:web:request /api/users"
                    "WARN:db:slow query detected"
                    "ERROR:web:500 internal error"
                    "INFO:auth:login success"
                    "ERROR:db:deadlock detected"
                    "INFO:web:request /api/items"
                    "WARN:auth:rate limit near"
                    "ERROR:auth:invalid token"
                    "INFO:web:request /api/status")))
        ;; Parse all logs
        (let ((records (mapcar 'neovm--map-adv-parse-log logs)))
          ;; Filter out nil (unparseable)
          (let ((valid (delq nil (copy-sequence records))))
            ;; Aggregate by level
            (let ((by-level (funcall 'neovm--map-adv-aggregate valid
                                     (lambda (r) (cdr (assq 'level r))))))
              ;; Aggregate by component
              (let ((by-comp (funcall 'neovm--map-adv-aggregate valid
                                      (lambda (r) (cdr (assq 'component r))))))
                ;; Extract error messages only
                (let ((errors (mapcan (lambda (r)
                                        (if (string= (cdr (assq 'level r)) "ERROR")
                                            (list (cdr (assq 'message r)))
                                          nil))
                                      valid)))
                  ;; Summary report
                  (list
                    (length valid)
                    by-level
                    by-comp
                    errors
                    ;; format summary
                    (mapconcat (lambda (pair)
                                 (format "%s: %d" (car pair) (cdr pair)))
                               by-level ", "))))))))
    (fmakunbound 'neovm--map-adv-parse-log)
    (fmakunbound 'neovm--map-adv-aggregate)))"#;
    assert_oracle_parity(form);
}
