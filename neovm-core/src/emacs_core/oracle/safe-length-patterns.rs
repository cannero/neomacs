//! Oracle parity tests for `safe-length` — advanced patterns:
//! proper lists, dotted/improper lists, circular lists of varying
//! cycle lengths, comparison with `length`, atoms, structural
//! validation, and complex data structure safety checks.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// safe-length on proper lists of many sizes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_proper_list_range() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test safe-length on proper lists from size 0 to 20,
    // verifying it matches length exactly each time.
    let form = r#"
(let ((results nil))
  (dotimes (n 21)
    (let ((lst (make-list n 'x)))
      (push (list n (safe-length lst) (length lst)
                  (= (safe-length lst) (length lst)))
            results)))
  (nreverse results))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length on dotted/improper lists of various shapes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_dotted_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
 ;; Simple dotted pair
 (safe-length '(a . b))
 ;; Two-element dotted list
 (safe-length '(a b . c))
 ;; Longer dotted list
 (safe-length '(1 2 3 4 5 . 6))
 ;; Dotted with various tail types
 (safe-length (cons 'x 42))
 (safe-length (cons 'x "string-tail"))
 (safe-length (cons 'x [vector-tail]))
 ;; Constructed dotted lists
 (safe-length (cons 1 (cons 2 (cons 3 'end))))
 ;; Compare: proper vs dotted with same "visible" length
 (let ((proper '(a b c))
       (dotted '(a b . c)))
   (list (safe-length proper) (safe-length dotted)))
 ;; Deeply nested dotted
 (safe-length (cons 'a (cons 'b (cons 'c (cons 'd 'e))))))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length on circular lists of various cycle lengths
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_circular_various() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build circular lists with different cycle lengths and verify
    // that safe-length always returns an integer (no hang).
    let form = r#"
(let ((results nil))
  ;; Cycle of length 1 (self-loop)
  (let ((c1 (cons 'a nil)))
    (setcdr c1 c1)
    (push (list 'cycle-1 (integerp (safe-length c1))) results))
  ;; Cycle of length 2
  (let ((c2 (list 'a 'b)))
    (setcdr (last c2) c2)
    (push (list 'cycle-2 (integerp (safe-length c2))) results))
  ;; Cycle of length 5
  (let ((c5 (list 1 2 3 4 5)))
    (setcdr (last c5) c5)
    (push (list 'cycle-5 (integerp (safe-length c5))) results))
  ;; Lasso shape: 3 elements lead-in, then cycle of 4
  (let ((lasso (list 'a 'b 'c 'd 'e 'f 'g)))
    (setcdr (last lasso) (nthcdr 3 lasso))
    (push (list 'lasso (integerp (safe-length lasso))) results))
  ;; Cycle of length 10
  (let ((c10 (make-list 10 'x)))
    (setcdr (last c10) c10)
    (push (list 'cycle-10 (integerp (safe-length c10))) results))
  ;; All safe-length values should be non-negative integers
  (let ((all-int t))
    (dolist (r results)
      (unless (eq (cadr r) t) (setq all-int nil)))
    (push (list 'all-integer all-int) results))
  (nreverse results))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length vs length on atoms and nil
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_atoms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
 ;; nil
 (safe-length nil)
 ;; Integers
 (safe-length 0)
 (safe-length 42)
 (safe-length -1)
 ;; Floats
 (safe-length 3.14)
 ;; Strings
 (safe-length "hello")
 (safe-length "")
 ;; Vectors
 (safe-length [1 2 3])
 (safe-length [])
 ;; Symbols
 (safe-length 'foo)
 (safe-length t)
 ;; Characters
 (safe-length ?a)
 ;; Bool-vector
 (safe-length (make-bool-vector 5 t))
 ;; Hash-table
 (safe-length (make-hash-table)))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length for detecting circular structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_detect_circular() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use safe-length + proper-list-p to classify list structures
    // without hanging on circular lists.
    let form = r#"
(progn
  (fset 'neovm--sl-classify
    (lambda (obj)
      "Classify OBJ as proper-list, dotted-list, circular, non-list, or empty."
      (cond
       ((null obj) 'empty)
       ((not (consp obj)) 'non-list)
       ((proper-list-p obj) 'proper-list)
       ;; If safe-length returns >= 0 but it's not a proper list,
       ;; it could be dotted or circular. Check by walking with hare-tortoise.
       (t (let ((slow obj)
                (fast obj)
                (circular nil)
                (steps 0))
            (while (and (consp fast) (consp (cdr fast)) (< steps 1000))
              (setq slow (cdr slow))
              (setq fast (cddr fast))
              (setq steps (1+ steps))
              (when (eq slow fast)
                (setq circular t)
                ;; Break the while loop
                (setq fast nil)
                (setq steps 1001)))
            (if circular 'circular 'dotted-list))))))

  (unwind-protect
      (let ((results nil))
        ;; Proper list
        (push (list '(1 2 3) (funcall 'neovm--sl-classify '(1 2 3))) results)
        ;; Empty
        (push (list nil (funcall 'neovm--sl-classify nil)) results)
        ;; Non-list
        (push (list 42 (funcall 'neovm--sl-classify 42)) results)
        (push (list "str" (funcall 'neovm--sl-classify "str")) results)
        ;; Dotted list
        (push (list '(a . b) (funcall 'neovm--sl-classify '(a . b))) results)
        (push (list '(1 2 . 3) (funcall 'neovm--sl-classify '(1 2 . 3))) results)
        ;; Circular list
        (let ((c (list 'x 'y 'z)))
          (setcdr (last c) c)
          (push (list 'circular-3 (funcall 'neovm--sl-classify c)) results))
        ;; Lasso circular
        (let ((lasso (list 'a 'b 'c 'd)))
          (setcdr (last lasso) (cdr lasso))
          (push (list 'lasso-circ (funcall 'neovm--sl-classify lasso)) results))
        (nreverse results))
    (fmakunbound 'neovm--sl-classify)))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: safe data structure validation combining safe-length with type checks
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_validation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a validator that uses safe-length to safely inspect
    // data structures of unknown shape.
    let form = r#"
(progn
  (fset 'neovm--sl-validate-record
    (lambda (rec)
      "Validate a record: must be a proper list of length 3,
       where car is a symbol (tag), cadr is a string (name),
       caddr is an integer (value)."
      (and (consp rec)
           (let ((sl (safe-length rec)))
             (and (integerp sl)
                  (= sl 3)
                  (proper-list-p rec)
                  (symbolp (nth 0 rec))
                  (stringp (nth 1 rec))
                  (integerp (nth 2 rec)))))))

  (fset 'neovm--sl-validate-table
    (lambda (tbl)
      "Validate a table: must be a proper list of valid records.
       Uses safe-length to guard against circular/improper structure."
      (and (or (null tbl) (consp tbl))
           (let ((sl (safe-length tbl)))
             (and (integerp sl)
                  (proper-list-p tbl)
                  (let ((ok t))
                    (dolist (rec tbl)
                      (unless (funcall 'neovm--sl-validate-record rec)
                        (setq ok nil)))
                    ok))))))

  (unwind-protect
      (let ((results nil))
        ;; Valid records
        (push (list 'valid-rec
                    (funcall 'neovm--sl-validate-record '(person "Alice" 30))
                    (funcall 'neovm--sl-validate-record '(item "Widget" 99)))
              results)
        ;; Invalid records
        (push (list 'invalid-rec
                    (funcall 'neovm--sl-validate-record '("notasym" "Bob" 25))
                    (funcall 'neovm--sl-validate-record '(person 42 30))
                    (funcall 'neovm--sl-validate-record '(person "Alice"))
                    (funcall 'neovm--sl-validate-record '(a b c d))
                    (funcall 'neovm--sl-validate-record 42)
                    (funcall 'neovm--sl-validate-record nil))
              results)
        ;; Valid table
        (push (list 'valid-tbl
                    (funcall 'neovm--sl-validate-table
                             '((person "Alice" 30)
                               (person "Bob" 25)
                               (item "Widget" 99)))
                    (funcall 'neovm--sl-validate-table nil))
              results)
        ;; Invalid table (contains bad record)
        (push (list 'invalid-tbl
                    (funcall 'neovm--sl-validate-table
                             '((person "Alice" 30)
                               (bad-record)
                               (item "Widget" 99)))
                    (funcall 'neovm--sl-validate-table
                             '((person "Alice" 30) . leftover)))
              results)
        ;; Circular table
        (let ((circ (list '(person "Alice" 30) '(person "Bob" 25))))
          (setcdr (last circ) circ)
          (push (list 'circular-tbl
                      (funcall 'neovm--sl-validate-table circ))
                results))
        (nreverse results))
    (fmakunbound 'neovm--sl-validate-record)
    (fmakunbound 'neovm--sl-validate-table)))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: safe-length in recursive tree size computation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_tree_size() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use safe-length to safely compute tree sizes, bailing out on
    // circular or malformed structures.
    let form = r#"
(progn
  (fset 'neovm--sl-tree-size
    (lambda (tree max-depth)
      "Count nodes in TREE, using safe-length to guard against
       circular structures. Returns (count . ok) where ok is t
       if no circular structure was detected."
      (cond
       ((<= max-depth 0) (cons 1 t))
       ((null tree) (cons 0 t))
       ((not (consp tree)) (cons 1 t))
       (t (let ((sl (safe-length tree)))
            (if (and (integerp sl) (proper-list-p tree))
                ;; Proper list: recurse into elements
                (let ((total 0) (ok t))
                  (dolist (elt tree)
                    (let ((sub (funcall 'neovm--sl-tree-size elt (1- max-depth))))
                      (setq total (+ total (car sub)))
                      (unless (cdr sub) (setq ok nil))))
                  (cons total ok))
              ;; Not a proper list or potentially circular
              (cons 1 nil)))))))

  (unwind-protect
      (list
       ;; Flat list
       (funcall 'neovm--sl-tree-size '(a b c d e) 10)
       ;; Nested tree
       (funcall 'neovm--sl-tree-size '(a (b c) (d (e f))) 10)
       ;; Deeply nested
       (funcall 'neovm--sl-tree-size '(((a))) 10)
       ;; Atom
       (funcall 'neovm--sl-tree-size 42 10)
       ;; nil
       (funcall 'neovm--sl-tree-size nil 10)
       ;; Dotted list (detected as non-proper)
       (funcall 'neovm--sl-tree-size '(a . b) 10)
       ;; Mixed proper and dotted
       (funcall 'neovm--sl-tree-size '(a (b . c) d) 10)
       ;; Large flat list
       (car (funcall 'neovm--sl-tree-size (make-list 50 'x) 10)))
    (fmakunbound 'neovm--sl-tree-size)))
"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length compared across progressively built and mutated lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_patterns_mutation_tracking() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Track safe-length as we cons, append, nconc, and destructively
    // modify a list.
    let form = r#"
(let ((results nil)
      (lst nil))
  ;; Build up with cons
  (dotimes (i 5)
    (setq lst (cons i lst))
    (push (list 'cons i (safe-length lst)) results))
  ;; Append creates a new list
  (let ((appended (append lst '(a b c))))
    (push (list 'append (safe-length appended)
                (safe-length lst))
          results))
  ;; nconc extends destructively
  (let ((ext (list 'x 'y)))
    (nconc lst ext)
    (push (list 'nconc (safe-length lst)) results))
  ;; setcdr to truncate
  (setcdr (nthcdr 2 lst) nil)
  (push (list 'truncated (safe-length lst)) results)
  ;; Make it dotted
  (setcdr (last lst) 'tail)
  (push (list 'dotted (safe-length lst)) results)
  ;; Verify dotted safe-length is one less than proper
  (let ((proper '(1 2 3 4 5))
        (dotted (cons 1 (cons 2 (cons 3 (cons 4 5))))))
    (push (list 'proper-vs-dotted
                (safe-length proper)
                (safe-length dotted))
          results))
  (nreverse results))
"#;
    assert_oracle_parity(form);
}
