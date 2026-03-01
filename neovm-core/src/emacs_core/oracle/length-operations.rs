//! Oracle parity tests for `length`, `safe-length`, `proper-list-p`,
//! `string-bytes`, `string-width`, and length comparison operations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// length on various types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_length_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (length nil)
                        (length '(a))
                        (length '(a b c))
                        (length '(1 2 3 4 5 6 7 8 9 10)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_length_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (length "")
                        (length "hello")
                        (length "café")
                        (length "日本語"))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_length_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (length [])
                        (length [1 2 3])
                        (length (make-vector 100 0)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// safe-length (handles circular/dotted lists)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_normal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (safe-length nil)
                        (safe-length '(a b c))
                        (safe-length '(1 2 3 4 5))
                        (safe-length "not a list")
                        (safe-length 42))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_safe_length_dotted() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Dotted list: (a b . c) has safe-length 2
    let form = r#"(list (safe-length '(a . b))
                        (safe-length '(a b . c))
                        (safe-length '(1 2 3 . 4)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// proper-list-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_proper_list_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (proper-list-p nil)
                        (proper-list-p '(a b c))
                        (proper-list-p '(1))
                        (proper-list-p '(a . b))
                        (proper-list-p '(a b . c))
                        (proper-list-p 42)
                        (proper-list-p "string")
                        (proper-list-p [vector]))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// string-bytes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_bytes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (string-bytes "")
                        (string-bytes "hello")
                        (string-bytes "café")
                        (string-bytes "日本語")
                        (string-bytes "\x00\x01\x02"))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_string_bytes_vs_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-bytes >= length; equality only for ASCII
    let form = r#"(let ((ascii "hello world")
                        (multi "héllo wörld")
                        (cjk "日本語テスト"))
                    (list (= (string-bytes ascii) (length ascii))
                          (> (string-bytes multi) (length multi))
                          (> (string-bytes cjk) (length cjk))
                          (- (string-bytes multi) (length multi))
                          (- (string-bytes cjk) (length cjk))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// string-width
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_width_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (string-width "")
                        (string-width "hello")
                        (string-width "café"))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_string_width_cjk() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // CJK characters are typically double-width
    let form = r#"(list (string-width "日本語")
                        (string-width "Abc")
                        (string-width "A日B本C語"))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: string statistics
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_length_string_statistics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compute various statistics about a list of strings
    let form = r#"(let ((strings '("hello" "world" "café" "日本語" "" "a")))
                    (let ((total-chars 0)
                          (total-bytes 0)
                          (total-width 0)
                          (max-len 0)
                          (min-len most-positive-fixnum)
                          (remaining strings))
                      (while remaining
                        (let* ((s (car remaining))
                               (len (length s)))
                          (setq total-chars (+ total-chars len)
                                total-bytes (+ total-bytes (string-bytes s))
                                total-width (+ total-width (string-width s))
                                max-len (max max-len len)
                                min-len (min min-len len)
                                remaining (cdr remaining))))
                      (list total-chars total-bytes total-width
                            max-len min-len
                            (length strings))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: group-by-length
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_length_group_by() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Group a list of strings by their length
    let form = r#"(let ((words '("a" "bb" "cc" "ddd" "ee" "f" "ggg" "hh")))
                    (let ((groups nil))
                      (dolist (w words)
                        (let* ((len (length w))
                               (existing (assq len groups)))
                          (if existing
                              (setcdr existing
                                      (append (cdr existing) (list w)))
                            (setq groups
                                  (cons (list len w) groups)))))
                      ;; Sort by length
                      (sort groups
                            (lambda (a b) (< (car a) (car b))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: proper-list validation pipeline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_proper_list_filter_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Filter and classify a heterogeneous collection
    let form = r#"(let ((items (list nil
                                    '(a b c)
                                    '(x . y)
                                    42
                                    "str"
                                    '(1 2 3 4)
                                    '(p q . r)
                                    [vec]
                                    '(single))))
                    (let ((proper nil)
                          (improper nil)
                          (non-list nil))
                      (dolist (item items)
                        (cond
                         ((proper-list-p item)
                          (setq proper
                                (cons (list item (safe-length item))
                                      proper)))
                         ((consp item)
                          (setq improper
                                (cons (list item (safe-length item))
                                      improper)))
                         (t
                          (setq non-list (cons item non-list)))))
                      (list (nreverse proper)
                            (nreverse improper)
                            (nreverse non-list))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: padded column formatting with string-width
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_width_column_format() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Format entries to align in columns based on display width
    let form = r#"(let ((entries '(("Name" "Age" "City")
                                    ("Alice" "30" "Boston")
                                    ("Bob" "25" "NYC"))))
                    ;; Compute max width per column
                    (let ((ncols (length (car entries)))
                          (col-widths nil))
                      (let ((i 0))
                        (while (< i ncols)
                          (let ((max-w 0))
                            (dolist (row entries)
                              (let ((w (string-width (nth i row))))
                                (when (> w max-w) (setq max-w w))))
                            (setq col-widths (append col-widths (list max-w))))
                          (setq i (1+ i))))
                      ;; Format each row
                      (mapcar
                       (lambda (row)
                         (let ((parts nil) (i 0))
                           (while (< i ncols)
                             (let* ((cell (nth i row))
                                    (pad (- (nth i col-widths)
                                            (string-width cell))))
                               (setq parts
                                     (cons (concat cell
                                                   (make-string
                                                    (max 0 pad) ?\ ))
                                           parts)))
                             (setq i (1+ i)))
                           (mapconcat #'identity (nreverse parts) " | ")))
                       entries)))"#;
    assert_oracle_parity(form);
}
