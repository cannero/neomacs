//! Comprehensive oracle parity tests for cons, list, and dotted pair operations.
//!
//! Covers: `cons` with all value types, `list` with 0-10+ args, dotted pairs
//! `(cons a b)` where b is non-nil non-list, `list*` behavior, deeply nested
//! cons cells, `proper-list-p` vs improper lists, `nthcdr` on dotted lists,
//! `length` vs `safe-length` on dotted/circular, `last` on dotted lists,
//! `butlast`/`nbutlast` on dotted, `copy-tree` vs `copy-sequence` depth,
//! `tree-equal` patterns, `make-list`, `number-sequence` cons integration.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// cons with all value types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cons_with_all_value_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; cons with integers
      (cons 1 2)
      (cons 0 0)
      (cons -1 -2)
      (cons most-positive-fixnum most-negative-fixnum)
      ;; cons with floats
      (cons 1.5 2.5)
      (cons 0.0 -0.0)
      (cons 1.0e10 1.0e-10)
      ;; cons with strings
      (cons "hello" "world")
      (cons "" "nonempty")
      (cons "a" nil)
      ;; cons with symbols
      (cons 'foo 'bar)
      (cons t nil)
      (cons nil nil)
      (cons nil t)
      ;; cons with characters
      (cons ?a ?z)
      (cons ?\n ?\t)
      ;; cons with vectors
      (cons [1 2 3] [4 5 6])
      (cons [] [])
      ;; cons with mixed types
      (cons 42 "forty-two")
      (cons 'sym [1 2])
      (cons ?a 3.14)
      (cons nil [])
      (cons t "true")
      ;; nested cons
      (cons (cons 1 2) (cons 3 4))
      (cons (cons (cons 'a 'b) 'c) 'd))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// list with 0-12 arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_list_arity_range() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; 0 args
      (list)
      ;; 1 arg
      (list 'a)
      ;; 2 args
      (list 1 2)
      ;; 3 args
      (list "x" "y" "z")
      ;; 4 args
      (list t nil t nil)
      ;; 5 args
      (list 1 2 3 4 5)
      ;; 6 args
      (list 'a 'b 'c 'd 'e 'f)
      ;; 7 args
      (list 1.0 2.0 3.0 4.0 5.0 6.0 7.0)
      ;; 8 args
      (list ?a ?b ?c ?d ?e ?f ?g ?h)
      ;; 10 args
      (list 0 1 2 3 4 5 6 7 8 9)
      ;; 12 args with mixed types
      (list 1 "two" 'three 4.0 ?5 [6] nil t '(7) '(8 . 9) 10 '(11 12))
      ;; Nested list calls
      (list (list 1 2) (list 3 4) (list 5 6))
      ;; list result is a proper list
      (proper-list-p (list 1 2 3))
      (proper-list-p (list))
      ;; length of list results
      (length (list 1 2 3 4 5))
      (length (list)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Dotted pairs: (cons a b) where b is non-nil non-list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dotted_pair_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((d1 (cons 'a 'b))
            (d2 (cons 1 2))
            (d3 (cons "hello" 42))
            (d4 (cons nil 'something))
            (d5 (cons 'x (cons 'y 'z))))
      (list
        ;; Basic dotted pair structure
        (car d1) (cdr d1)
        (car d2) (cdr d2)
        (car d3) (cdr d3)
        (car d4) (cdr d4)
        ;; Dotted pair chain: (x . (y . z))
        (car d5) (car (cdr d5)) (cdr (cdr d5))
        ;; Printing representation via prin1-to-string
        (prin1-to-string d1)
        (prin1-to-string d2)
        (prin1-to-string d5)
        ;; consp and listp on dotted pairs
        (consp d1)
        (listp d1)
        (consp d2)
        ;; atom on non-cons cdr
        (atom (cdr d1))
        (atom (cdr d2))
        ;; proper-list-p on dotted pairs
        (proper-list-p d1)
        (proper-list-p d5)
        ;; Comparison
        (equal (cons 'a 'b) (cons 'a 'b))
        (equal (cons 1 2) (cons 1 2))
        (eq (cons 'a 'b) (cons 'a 'b))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// list* (aka cons*) behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_list_star_behavior() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // list* is defined in cl-lib, test via require
    let form = r#"(progn
      (require 'cl-lib)
      (list
        ;; 1 arg: returns the arg itself
        (cl-list* 42)
        (cl-list* nil)
        (cl-list* 'a)
        ;; 2 args: equivalent to cons
        (cl-list* 1 2)
        (equal (cl-list* 1 2) (cons 1 2))
        ;; 3 args: (cons a (cons b c))
        (cl-list* 1 2 3)
        (equal (cl-list* 1 2 3) (cons 1 (cons 2 3)))
        ;; 4 args
        (cl-list* 'a 'b 'c 'd)
        (equal (cl-list* 'a 'b 'c 'd) '(a b c . d))
        ;; last arg is a list -> produces a proper list
        (cl-list* 1 2 '(3 4 5))
        (equal (cl-list* 1 2 '(3 4 5)) '(1 2 3 4 5))
        ;; last arg is nil -> same as list without last arg
        (cl-list* 'a 'b 'c nil)
        (equal (cl-list* 'a 'b 'c nil) (list 'a 'b 'c))
        ;; last arg is a dotted pair
        (cl-list* 1 '(2 . 3))
        (equal (cl-list* 1 '(2 . 3)) '(1 2 . 3))
        ;; deeply nested
        (cl-list* 1 2 3 4 5 '(6 7))
        (proper-list-p (cl-list* 1 2 3 4 5 '(6 7)))
        (proper-list-p (cl-list* 1 2 3))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Deeply nested cons cells
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_deeply_nested_cons() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
      ;; Build a deeply nested structure: ((((1 . 2) . 3) . 4) . 5)
      (let ((deep (cons 1 2)))
        (setq deep (cons deep 3))
        (setq deep (cons deep 4))
        (setq deep (cons deep 5))
        (list
          deep
          (car deep)
          (cdr deep)
          (caar deep)
          (cdar deep)
          (caaar deep)
          (cdaar deep)
          ;; Verify structure
          (equal deep '((((1 . 2) . 3) . 4) . 5))
          ;; Build right-leaning: (1 . (2 . (3 . (4 . 5))))
          (let ((right (cons 4 5)))
            (setq right (cons 3 right))
            (setq right (cons 2 right))
            (setq right (cons 1 right))
            (list
              right
              ;; This is basically a dotted list (1 2 3 4 . 5)
              (equal right '(1 2 3 4 . 5))
              (car right)
              (cadr right)
              (caddr right)
              (cadddr right)
              (cddddr right)
              (proper-list-p right)
              ;; nthcdr on right-leaning structure
              (nthcdr 0 right)
              (nthcdr 1 right)
              (nthcdr 2 right)
              (nthcdr 3 right)
              (nthcdr 4 right))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// proper-list-p vs improper lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_proper_list_p_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; Proper lists
      (proper-list-p nil)
      (proper-list-p '())
      (proper-list-p '(1))
      (proper-list-p '(1 2 3))
      (proper-list-p '(a b c d e))
      (proper-list-p (list 1 2 3))
      (proper-list-p (make-list 5 'x))
      ;; Improper (dotted) lists
      (proper-list-p '(1 . 2))
      (proper-list-p '(1 2 . 3))
      (proper-list-p '(1 2 3 . 4))
      (proper-list-p (cons 'a 'b))
      (proper-list-p (cons 1 (cons 2 3)))
      ;; Non-list types
      (proper-list-p 42)
      (proper-list-p "hello")
      (proper-list-p 'sym)
      (proper-list-p [1 2 3])
      (proper-list-p t)
      ;; Nested proper lists (the outer list is still proper)
      (proper-list-p '((1 2) (3 4) (5 6)))
      (proper-list-p '((a . b) (c . d)))
      ;; Circular lists via safe-length (not proper-list-p which loops)
      ;; We test safe-length instead for circular
      (let ((circ (list 1 2 3)))
        (setcdr (last circ) circ)
        (safe-length circ))
      ;; Circular dotted list
      (let ((circ (cons 1 (cons 2 nil))))
        (setcdr (cdr circ) circ)
        (safe-length circ)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// nthcdr on dotted lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nthcdr_on_dotted_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; nthcdr on proper list
      (nthcdr 0 '(a b c))
      (nthcdr 1 '(a b c))
      (nthcdr 2 '(a b c))
      (nthcdr 3 '(a b c))
      (nthcdr 4 '(a b c))
      ;; nthcdr on dotted list
      (nthcdr 0 '(a b . c))
      (nthcdr 1 '(a b . c))
      (nthcdr 2 '(a b . c))
      ;; nthcdr 3 on (a b . c) should error or return c's cdr => error
      ;; nthcdr on single dotted pair
      (nthcdr 0 '(x . y))
      (nthcdr 1 '(x . y))
      ;; nthcdr 0 returns the list itself
      (let ((lst '(1 2 3 . 4)))
        (eq (nthcdr 0 lst) lst))
      ;; nthcdr on nil
      (nthcdr 0 nil)
      (nthcdr 1 nil)
      (nthcdr 100 nil)
      ;; nthcdr on long dotted list
      (nthcdr 3 '(a b c d e . f))
      (nthcdr 5 '(a b c d e . f))
      ;; nth on dotted list (nth calls nthcdr then car)
      (nth 0 '(a b . c))
      (nth 1 '(a b . c))
      ;; nth beyond proper portion
      (nth 0 '(10 20 30 . 40))
      (nth 1 '(10 20 30 . 40))
      (nth 2 '(10 20 30 . 40)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// length vs safe-length on dotted and circular lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_length_vs_safe_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; length on proper lists
      (length nil)
      (length '(1))
      (length '(1 2 3))
      (length '(a b c d e f g h i j))
      ;; safe-length on proper lists (same as length)
      (safe-length nil)
      (safe-length '(1))
      (safe-length '(1 2 3))
      (safe-length '(a b c d e f g h i j))
      ;; safe-length on dotted lists (counts proper portion)
      (safe-length '(a . b))
      (safe-length '(a b . c))
      (safe-length '(1 2 3 . 4))
      (safe-length '(x y z w . v))
      ;; safe-length on circular lists (returns count before cycle detected)
      (let ((c (list 1 2 3)))
        (setcdr (last c) c)
        (safe-length c))
      ;; Circular list of length 1
      (let ((c (list 'a)))
        (setcdr c c)
        (safe-length c))
      ;; Circular list of length 5
      (let ((c (list 1 2 3 4 5)))
        (setcdr (last c) c)
        (safe-length c))
      ;; Circular starting not at head
      (let ((c (list 1 2 3 4)))
        (setcdr (last c) (cdr c))
        (safe-length c))
      ;; safe-length on non-list
      (safe-length 42)
      (safe-length "hello")
      (safe-length 'sym))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// last on dotted lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_last_on_various_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; last on proper lists
      (last '(1 2 3))
      (last '(a))
      (last nil)
      (last '(x y z w))
      ;; last with N argument
      (last '(1 2 3 4 5) 1)
      (last '(1 2 3 4 5) 2)
      (last '(1 2 3 4 5) 3)
      (last '(1 2 3 4 5) 5)
      (last '(1 2 3 4 5) 6)
      (last '(1 2 3 4 5) 0)
      ;; last on dotted lists
      (last '(a b . c))
      (last '(1 . 2))
      (last '(x y z . w))
      ;; last with N on dotted lists
      (last '(a b c . d) 1)
      (last '(a b c . d) 2)
      (last '(a b c . d) 3)
      (last '(a b c . d) 4)
      ;; car/cdr of last
      (car (last '(1 2 3)))
      (cdr (last '(1 2 3)))
      (car (last '(a b . c)))
      (cdr (last '(a b . c))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// butlast and nbutlast on dotted and proper lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_butlast_nbutlast_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; butlast on proper lists
      (butlast '(1 2 3))
      (butlast '(1 2 3) 1)
      (butlast '(1 2 3) 2)
      (butlast '(1 2 3) 3)
      (butlast '(1 2 3) 4)
      (butlast '(1 2 3) 0)
      (butlast '(a))
      (butlast nil)
      ;; butlast on longer list
      (butlast '(1 2 3 4 5 6 7) 3)
      (butlast '(1 2 3 4 5 6 7) 0)
      ;; nbutlast on copies (since it's destructive)
      (let ((l (list 1 2 3 4 5)))
        (nbutlast l))
      (let ((l (list 1 2 3 4 5)))
        (nbutlast l 2))
      (let ((l (list 1 2 3 4 5)))
        (nbutlast l 5))
      (let ((l (list 1 2 3 4 5)))
        (nbutlast l 0))
      (let ((l (list 'a)))
        (nbutlast l))
      ;; Verify butlast doesn't modify original
      (let ((orig '(1 2 3 4 5)))
        (let ((result (butlast orig 2)))
          (list result orig (equal orig '(1 2 3 4 5)))))
      ;; butlast vs nbutlast equivalence
      (let ((l1 '(a b c d e))
            (l2 (list 'a 'b 'c 'd 'e)))
        (equal (butlast l1 2) (nbutlast l2 2))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// copy-tree vs copy-sequence depth
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_copy_tree_vs_copy_sequence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
      ;; copy-sequence: shallow copy — nested structure is shared
      (let* ((inner (list 1 2 3))
             (outer (list inner 'b 'c))
             (copied (copy-sequence outer)))
        (list
          (equal outer copied)
          ;; Shallow: inner list is the SAME object
          (eq (car outer) (car copied))
          ;; Modifying inner affects both
          (progn (setcar inner 999)
                 (list (car (car outer)) (car (car copied))))))

      ;; copy-tree: deep copy — nested structure is independent
      (let* ((inner (list 1 2 3))
             (outer (list inner 'b 'c))
             (copied (copy-tree outer)))
        (list
          (equal outer copied)
          ;; Deep: inner list is a DIFFERENT object
          (eq (car outer) (car copied))
          ;; Modifying original inner does NOT affect copy
          (progn (setcar inner 888)
                 (list (car (car outer)) (car (car copied))))))

      ;; copy-tree on dotted pairs
      (let* ((d '((1 . 2) . (3 . 4)))
             (c (copy-tree d)))
        (list (equal d c)
              (eq d c)
              (eq (car d) (car c))
              (eq (cdr d) (cdr c))))

      ;; copy-tree on deeply nested
      (let* ((deep '((((a . b) . c) . d) . e))
             (c (copy-tree deep)))
        (list (equal deep c)
              (eq (caaar deep) (caaar c))  ;; symbols are eq
              (eq (caar deep) (caar c))))  ;; but cons cells are not

      ;; copy-tree on vectors (does NOT recurse into vectors)
      (let* ((v (list [1 2 3] 'a))
             (c (copy-tree v)))
        (list (equal v c)
              ;; Vector is shared even in copy-tree
              (eq (car v) (car c))))

      ;; copy-sequence on various types
      (let ((lst (list 1 2 3))
            (str "hello")
            (vec [1 2 3]))
        (list
          (equal (copy-sequence lst) lst)
          (eq (copy-sequence lst) lst)
          (equal (copy-sequence str) str)
          (eq (copy-sequence str) str)
          (equal (copy-sequence vec) vec)
          (eq (copy-sequence vec) vec))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// tree-equal patterns (cl-lib)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tree_equal_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
      (require 'cl-lib)
      (list
        ;; Simple tree equality
        (cl-tree-equal '(1 2 3) '(1 2 3))
        (cl-tree-equal '(a b c) '(a b c))
        (cl-tree-equal nil nil)
        (cl-tree-equal '(1 . 2) '(1 . 2))
        ;; Different structures
        (cl-tree-equal '(1 2 3) '(1 2 4))
        (cl-tree-equal '(1 2) '(1 2 3))
        (cl-tree-equal '(a . b) '(a . c))
        ;; Nested trees
        (cl-tree-equal '((1 2) (3 4)) '((1 2) (3 4)))
        (cl-tree-equal '((1 2) (3 4)) '((1 2) (3 5)))
        (cl-tree-equal '(((a))) '(((a))))
        (cl-tree-equal '(((a))) '(((b))))
        ;; Mixed nesting
        (cl-tree-equal '(1 (2 (3 (4)))) '(1 (2 (3 (4)))))
        (cl-tree-equal '(1 (2 (3 (4)))) '(1 (2 (3 (5)))))
        ;; Atoms
        (cl-tree-equal 'a 'a)
        (cl-tree-equal 42 42)
        (cl-tree-equal 'a 'b)
        ;; Trees with dotted pairs
        (cl-tree-equal '((1 . 2) . (3 . 4)) '((1 . 2) . (3 . 4)))
        (cl-tree-equal '((1 . 2) . (3 . 4)) '((1 . 2) . (3 . 5)))
        ;; nil vs empty list
        (cl-tree-equal nil '())
        (cl-tree-equal '() nil)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// make-list and number-sequence into cons structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_make_list_and_number_sequence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; make-list basics
      (make-list 0 'x)
      (make-list 1 'x)
      (make-list 5 0)
      (make-list 3 nil)
      (make-list 4 t)
      (make-list 3 '(a b))
      ;; make-list shares the init object
      (let ((ml (make-list 3 '(1 2))))
        (list ml
              (eq (nth 0 ml) (nth 1 ml))
              (eq (nth 1 ml) (nth 2 ml))))
      ;; number-sequence basics
      (number-sequence 1 5)
      (number-sequence 1 5 2)
      (number-sequence 5 1 -1)
      (number-sequence 0 0)
      (number-sequence 1 1)
      (number-sequence 0 10 3)
      (number-sequence -5 5 2)
      ;; number-sequence produces proper list
      (proper-list-p (number-sequence 1 10))
      (length (number-sequence 1 10))
      ;; Combining: make-list then nconc with number-sequence
      (nconc (make-list 3 'x) (number-sequence 1 3))
      ;; append
      (append (make-list 2 'a) (number-sequence 10 12) nil)
      ;; reverse of number-sequence
      (reverse (number-sequence 1 5))
      (equal (reverse (number-sequence 1 5)) (number-sequence 5 1 -1)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Cons cell mutation: setcar/setcdr on various structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cons_mutation_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; setcar on first element
      (let ((l (list 1 2 3)))
        (setcar l 'a)
        l)
      ;; setcdr to create dotted pair
      (let ((l (list 1 2 3)))
        (setcdr l 'z)
        l)
      ;; setcar on nested
      (let ((l (list (list 'a 'b) (list 'c 'd))))
        (setcar (car l) 'X)
        l)
      ;; Chain of setcdr to build a dotted list
      (let ((l (list 1 2 3 4)))
        (setcdr (cddr l) 99)
        l)
      ;; setcar/setcdr on dotted pair
      (let ((d (cons 'a 'b)))
        (setcar d 'x)
        (setcdr d 'y)
        d)
      ;; Building a list manually with cons and setcdr
      (let ((head (cons 1 nil)))
        (setcdr head (cons 2 nil))
        (setcdr (cdr head) (cons 3 nil))
        (list head (proper-list-p head) (length head)))
      ;; Mutation doesn't affect copies
      (let* ((orig (list 1 2 3))
             (copy (copy-sequence orig)))
        (setcar orig 'changed)
        (list orig copy))
      ;; Multiple mutations
      (let ((l (list 'a 'b 'c 'd 'e)))
        (setcar l 1)
        (setcar (cdr l) 2)
        (setcar (cddr l) 3)
        (setcar (nthcdr 3 l) 4)
        (setcar (nthcdr 4 l) 5)
        l))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Association list operations on cons-based alists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cons_alist_integration() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((alist (list (cons 'a 1) (cons 'b 2) (cons 'c 3)
                          (cons 'a 10) (cons 'd 4))))
      (list
        ;; assq finds first match
        (assq 'a alist)
        (assq 'b alist)
        (assq 'c alist)
        (assq 'd alist)
        (assq 'e alist)
        ;; assoc with string keys
        (let ((sal (list (cons "x" 1) (cons "y" 2) (cons "z" 3))))
          (list (assoc "x" sal) (assoc "y" sal) (assoc "z" sal) (assoc "w" sal)))
        ;; rassq / rassoc
        (rassq 1 alist)
        (rassq 2 alist)
        (rassq 99 alist)
        ;; Building alist with cons
        (let ((al nil))
          (setq al (cons (cons 'name "Alice") al))
          (setq al (cons (cons 'age 30) al))
          (setq al (cons (cons 'city "NYC") al))
          (list al
                (cdr (assq 'name al))
                (cdr (assq 'age al))
                (cdr (assq 'city al))))
        ;; Alist with dotted pair entries vs list entries
        (let ((dotted-alist '((a . 1) (b . 2)))
              (list-alist '((a 1) (b 2))))
          (list (assq 'a dotted-alist)
                (assq 'a list-alist)
                (cdr (assq 'a dotted-alist))
                (cdr (assq 'a list-alist))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// append, nconc, and cons list combination
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_append_nconc_cons_combinations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; append with proper lists
      (append '(1 2) '(3 4))
      (append '(a) '(b) '(c))
      (append nil '(1 2 3))
      (append '(1 2 3) nil)
      (append nil nil)
      ;; append with dotted list as last arg
      (append '(1 2) '(3 . 4))
      (append '(a b) 'c)
      (append nil 'x)
      ;; append with non-list as last arg
      (append '(1 2 3) 4)
      (append '(a) 'b)
      ;; nconc on copies
      (let ((a (list 1 2)) (b (list 3 4)))
        (nconc a b)
        a)
      (let ((a (list 'x)) (b (list 'y)) (c (list 'z)))
        (nconc a b c)
        a)
      ;; nconc with nil
      (let ((a nil) (b (list 1 2)))
        (nconc a b))
      ;; Verify append creates new cons cells
      (let ((l1 '(1 2)) (l2 '(3 4)))
        (let ((result (append l1 l2)))
          (list result
                (eq l1 result)
                ;; last arg is shared
                (eq l2 (cddr result)))))
      ;; append with many args
      (append '(1) '(2) '(3) '(4) '(5) '(6) '(7) '(8) '(9) '(10))
      ;; Nested append
      (append (append '(1 2) '(3)) (append '(4) '(5 6))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// memq, member, delq, delete on cons structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_membership_operations_on_cons() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
      ;; memq uses eq
      (memq 'b '(a b c d))
      (memq 'x '(a b c d))
      (memq nil '(a nil b))
      (memq t '(nil t nil))
      ;; member uses equal
      (member "b" '("a" "b" "c"))
      (member '(1 2) '((1 2) (3 4)))
      (member 3 '(1 2 3 4))
      ;; memq returns tail from match
      (let ((tail (memq 'c '(a b c d e))))
        (list tail (length tail) (car tail)))
      ;; delq: non-destructive-style but actually mutates
      (let ((l (list 'a 'b 'c 'b 'd)))
        (delq 'b l))
      (let ((l (list 1 2 3 2 1)))
        (delq 2 l))
      (delq 'x '(a b c))
      (delq nil (list 1 nil 2 nil 3))
      ;; delete uses equal
      (let ((l (list "a" "b" "c" "b")))
        (delete "b" l))
      (let ((l (list '(1) '(2) '(1) '(3))))
        (delete '(1) l))
      ;; Verify memq/member on dotted lists -- they work on proper portion
      (memq 'a '(a b . c))
      (memq 'b '(a b . c))
      (member 2 '(1 2 . 3)))"#;
    assert_oracle_parity(form);
}
