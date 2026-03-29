//! Oracle parity tests for comprehensive assignment forms:
//! `setq` multiple pairs, `setq-local`, `setq-default`, `set` with computed
//! symbol, `setf` on generalized places (`car`/`cdr`/`aref`/`nth`/`gethash`/
//! `symbol-value`/`symbol-function`/`symbol-plist`), `push`/`pop`/`cl-pushnew`,
//! `cl-incf`/`cl-decf`, and complex mutation patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// setq: multiple pairs with cross-referencing values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_multiple_pairs_cross_reference() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setq evaluates left-to-right; later pairs can reference earlier assignments
    let form = r#"(let ((a 0) (b 0) (c 0) (d 0) (e 0))
                    (setq a 3
                          b (* a a)         ;; b = 9
                          c (+ a b)         ;; c = 12
                          d (- c b)         ;; d = 3
                          e (list a b c d)) ;; e = (3 9 12 3)
                    (list a b c d e
                          ;; setq returns the last value
                          (setq a 100 b 200 c 300)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// set: assign via computed symbol name
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_set_with_computed_symbol() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // `set` takes a symbol (not a variable name) and assigns its value cell
    let form = r#"(progn
  (defvar neovm--test-set-target-a nil)
  (defvar neovm--test-set-target-b nil)
  (unwind-protect
      (let ((syms '(neovm--test-set-target-a neovm--test-set-target-b)))
        ;; Assign via `set` using computed symbols
        (let ((i 10))
          (dolist (s syms)
            (set s (* i i))
            (setq i (+ i 10))))
        (list (symbol-value 'neovm--test-set-target-a)
              (symbol-value 'neovm--test-set-target-b)
              ;; set returns the assigned value
              (set 'neovm--test-set-target-a 'reset)
              neovm--test-set-target-a))
    (makunbound 'neovm--test-set-target-a)
    (makunbound 'neovm--test-set-target-b)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// setf on car and cdr: in-place mutation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_car_cdr_mutation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on car/cdr mutates cons cells in place
    let form = r#"(let ((xs (list 1 2 3 4 5)))
                    (setf (car xs) 'first)
                    (setf (cdr (last xs)) '(extra tail))
                    (setf (cadr xs) 'second)
                    (setf (nth 2 xs) 'third)
                    ;; Verify the entire mutated structure
                    (let ((result (copy-sequence xs)))
                      (list result
                            (car xs)
                            (cadr xs)
                            (nth 2 xs)
                            (length xs))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// setf on aref: vector mutation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_aref_vector_mutation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on aref mutates vector elements in place
    let form = r#"(let ((v (vector 'a 'b 'c 'd 'e 'f 'g 'h)))
                    ;; Swap first and last
                    (let ((tmp (aref v 0)))
                      (setf (aref v 0) (aref v 7))
                      (setf (aref v 7) tmp))
                    ;; Set middle elements to computed values
                    (setf (aref v 3) (+ (length v) 100))
                    (setf (aref v 4) (concat "idx-" (number-to-string 4)))
                    ;; Build result from the mutated vector
                    (list (aref v 0) (aref v 1) (aref v 2) (aref v 3)
                          (aref v 4) (aref v 5) (aref v 6) (aref v 7)
                          (length v)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// setf on gethash: hash table place mutation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_gethash_place() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on gethash is the idiomatic way to puthash
    let form = r#"(let ((h (make-hash-table :test 'equal)))
                    (setf (gethash "name" h) "Alice")
                    (setf (gethash "age" h) 30)
                    (setf (gethash "scores" h) (list 95 87 92))
                    ;; Overwrite
                    (setf (gethash "age" h) (1+ (gethash "age" h)))
                    ;; Append to list value
                    (setf (gethash "scores" h)
                          (append (gethash "scores" h) '(100)))
                    (list (gethash "name" h)
                          (gethash "age" h)
                          (gethash "scores" h)
                          (hash-table-count h)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// setf on symbol-function / symbol-plist
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_symbol_function_and_plist() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on symbol-function is like fset; setf on symbol-plist replaces the plist
    let form = r#"(progn
  (unwind-protect
      (progn
        (setf (symbol-function 'neovm--test-setf-fn)
              (lambda (x) (* x x x)))
        (setf (symbol-plist 'neovm--test-setf-fn)
              '(doc "cubes a number" version 2))
        (list
          (funcall 'neovm--test-setf-fn 3)
          (funcall 'neovm--test-setf-fn 5)
          (symbol-plist 'neovm--test-setf-fn)
          (get 'neovm--test-setf-fn 'doc)
          (get 'neovm--test-setf-fn 'version)
          (fboundp 'neovm--test-setf-fn)))
    (fmakunbound 'neovm--test-setf-fn)
    (setplist 'neovm--test-setf-fn nil)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// push / pop on list places
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_push_pop_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // push adds to front, pop removes from front and returns the element
    let form = r#"(let ((stack nil)
                        (results nil))
                    ;; Push several items
                    (push 'a stack)
                    (push 'b stack)
                    (push 'c stack)
                    (push 'd stack)
                    (setq results (cons (copy-sequence stack) results))
                    ;; Pop items
                    (let ((p1 (pop stack))
                          (p2 (pop stack)))
                      (setq results (cons (list 'popped p1 p2) results))
                      (setq results (cons (copy-sequence stack) results)))
                    ;; Push onto a nested place (alist value)
                    (let ((data (list (cons 'items nil) (cons 'log nil))))
                      (push 'x (cdr (assq 'items data)))
                      (push 'y (cdr (assq 'items data)))
                      (push "entry-1" (cdr (assq 'log data)))
                      (setq results (cons data results)))
                    (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// cl-pushnew: conditional push (no duplicates)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_cl_pushnew() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cl-pushnew only pushes if item is not already a member
    let form = r#"(progn
  (require 'cl-lib)
  (let ((xs '(a b c)))
        ;; Push new element
        (cl-pushnew 'd xs)
        (let ((after-new (copy-sequence xs)))
          ;; Push existing element -- should not duplicate
          (cl-pushnew 'b xs)
          (let ((after-dup (copy-sequence xs)))
            ;; Push with :test 'equal for string keys
            (let ((strs (list "hello" "world")))
              (cl-pushnew "hello" strs :test 'equal)
              (cl-pushnew "new" strs :test 'equal)
              (list after-new
                    after-dup
                    (length after-new)
                    (length after-dup)
                    strs
                    (length strs)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// cl-incf / cl-decf on various places
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_incf_decf_places() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cl-incf and cl-decf on simple variables, vector elements, hash values
    let form = r#"(progn
  (require 'cl-lib)
  (let ((counter 0)
        (v (vector 10 20 30))
        (h (make-hash-table)))
    (puthash 'score 100 h)
    ;; incf on simple var
    (cl-incf counter)
    (cl-incf counter 5)
    (let ((c1 counter))
      ;; decf on simple var
      (cl-decf counter 2)
      (let ((c2 counter))
        ;; incf on vector element
        (cl-incf (aref v 1) 100)
        ;; decf on vector element
        (cl-decf (aref v 2) 15)
        ;; incf on hash value
        (cl-incf (gethash 'score h) 50)
        (cl-decf (gethash 'score h) 25)
        ;; incf on car of cons
        (let ((cell (cons 0 nil)))
          (cl-incf (car cell) 42)
          (list c1 c2 counter
                (aref v 0) (aref v 1) (aref v 2)
                (gethash 'score h)
                (car cell)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// setf on nth: deep list mutation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_nth_deep_mutation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on nth mutates the nth element of a list
    let form = r#"(let ((matrix (list (list 1 2 3)
                                       (list 4 5 6)
                                       (list 7 8 9))))
                    ;; Set diagonal to 0
                    (setf (nth 0 (nth 0 matrix)) 0)
                    (setf (nth 1 (nth 1 matrix)) 0)
                    (setf (nth 2 (nth 2 matrix)) 0)
                    ;; Set corners
                    (setf (nth 2 (nth 0 matrix)) 'tr)
                    (setf (nth 0 (nth 2 matrix)) 'bl)
                    matrix)"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: setf-driven adjacency list graph builder
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_graph_adjacency_builder() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a graph using setf on hash-table places, then traverse
    let form = r#"(let ((graph (make-hash-table :test 'eq)))
                    ;; Add edges: node -> list of neighbors
                    (setf (gethash 'a graph) '(b c))
                    (setf (gethash 'b graph) '(a d))
                    (setf (gethash 'c graph) '(a d e))
                    (setf (gethash 'd graph) '(b c))
                    (setf (gethash 'e graph) '(c))
                    ;; Add new edge: a -> e using push on gethash place
                    (push 'e (gethash 'a graph))
                    ;; BFS from 'a to find reachable nodes
                    (let ((visited (make-hash-table :test 'eq))
                          (queue (list 'a))
                          (order nil))
                      (puthash 'a t visited)
                      (while queue
                        (let ((node (pop queue)))
                          (push node order)
                          (dolist (neighbor (gethash node graph))
                            (unless (gethash neighbor visited)
                              (puthash neighbor t visited)
                              (setq queue (append queue (list neighbor)))))))
                      (list
                        ;; BFS order from a
                        (nreverse order)
                        ;; Adjacency: a's neighbors after push
                        (sort (copy-sequence (gethash 'a graph))
                              (lambda (x y) (string< (symbol-name x) (symbol-name y))))
                        ;; Degree of each node
                        (let ((degrees nil))
                          (maphash (lambda (k v) (push (cons k (length v)) degrees)) graph)
                          (sort degrees (lambda (a b) (string< (symbol-name (car a))
                                                                (symbol-name (car b)))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: setf with symbol-value in dynamic binding context
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_setq_setf_symbol_value_dynamic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setf on symbol-value bypasses lexical scope, accesses the dynamic binding
    let form = r#"(progn
  (defvar neovm--test-dynvar 'global-val)
  (unwind-protect
      (let ((results nil))
        ;; Read the global value
        (push (symbol-value 'neovm--test-dynvar) results)
        ;; Dynamic let-binding
        (let ((neovm--test-dynvar 'let-bound))
          (push (symbol-value 'neovm--test-dynvar) results)
          ;; setf on symbol-value changes the current dynamic binding
          (setf (symbol-value 'neovm--test-dynvar) 'setf-changed)
          (push (symbol-value 'neovm--test-dynvar) results)
          (push neovm--test-dynvar results))
        ;; After let exits, back to global
        (push (symbol-value 'neovm--test-dynvar) results)
        (nreverse results))
    (makunbound 'neovm--test-dynvar)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
