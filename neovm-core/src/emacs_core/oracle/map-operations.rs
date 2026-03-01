//! Oracle parity tests for mapping operations: `mapc`, `mapcan`,
//! `mapconcat`, `cl-mapcar` (multi-list), and `map-char-table`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// mapc (like mapcar but returns the original list)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_mapc_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapc returns the original list, not collected results
    let form = r#"(let ((log nil))
                    (let ((result (mapc (lambda (x)
                                          (setq log (cons (* x x) log)))
                                        '(1 2 3 4 5))))
                      (list result (nreverse log))))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_mapc_side_effects() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapc is used for side effects: populate a hash table
    let form = r#"(let ((h (make-hash-table :test 'equal)))
                    (mapc (lambda (pair)
                            (puthash (car pair) (cdr pair) h))
                          '(("a" . 1) ("b" . 2) ("c" . 3)))
                    (list (gethash "a" h)
                          (gethash "b" h)
                          (gethash "c" h)
                          (hash-table-count h)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// mapconcat with various separators
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_mapconcat_separator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
                    (mapconcat #'symbol-name '(a b c) ", ")
                    (mapconcat #'number-to-string '(1 2 3 4 5) "-")
                    (mapconcat #'identity '("hello" "world") " ")
                    (mapconcat #'upcase '("foo" "bar" "baz") "::"))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_mapconcat_empty_separator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
                    (mapconcat #'identity '("a" "b" "c") "")
                    (mapconcat (lambda (n) (format "%02d" n))
                               '(1 2 3) ""))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_mapconcat_complex_transform() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build SQL-like WHERE clause
    let form = r#"(let ((conditions '(("name" . "Alice")
                                       ("age" . "30")
                                       ("city" . "Boston"))))
                    (mapconcat
                     (lambda (pair)
                       (format "%s = '%s'" (car pair) (cdr pair)))
                     conditions
                     " AND "))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// mapcar with index tracking
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_mapcar_with_index() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Emulate mapcar-with-index using a counter
    let form = r#"(let ((idx -1))
                    (mapcar (lambda (x)
                              (setq idx (1+ idx))
                              (cons idx x))
                            '(a b c d e)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_mapcar_nested_transform() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Transform nested structure
    let form = r#"(mapcar
                    (lambda (row)
                      (mapcar (lambda (cell)
                                (if (numberp cell)
                                    (* cell 2)
                                  (upcase cell)))
                              row))
                    '(("name" 1 2) ("data" 3 4) ("info" 5 6)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: map-based data pipeline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Pipeline: parse → filter → transform → format
    let form = r#"(let ((raw '("Alice:30:dev" "Bob:25:qa" "Carol:35:dev"
                               "Dave:28:ops" "Eve:32:dev")))
                    ;; Parse into records
                    (let ((records
                           (mapcar
                            (lambda (line)
                              (let ((parts (split-string line ":")))
                                (list (nth 0 parts)
                                      (string-to-number (nth 1 parts))
                                      (nth 2 parts))))
                            raw)))
                      ;; Filter: only dev team
                      (let ((devs nil))
                        (mapc (lambda (r)
                                (when (string= (nth 2 r) "dev")
                                  (setq devs (cons r devs))))
                              records)
                        (setq devs (nreverse devs))
                        ;; Transform: add seniority label
                        (let ((labeled
                               (mapcar
                                (lambda (r)
                                  (list (nth 0 r)
                                        (nth 1 r)
                                        (if (>= (nth 1 r) 30)
                                            "senior" "junior")))
                                devs)))
                          ;; Format output
                          (mapconcat
                           (lambda (r)
                             (format "%s (%s, %s)"
                                     (nth 0 r)
                                     (nth 2 r)
                                     (number-to-string (nth 1 r))))
                           labeled
                           "; ")))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: parallel map with zip
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_map_zip_combine() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Zip two lists and apply function to pairs
    let form = r#"(let ((keys '(name age city role))
                        (vals '("Alice" 30 "Boston" "dev")))
                    ;; Manual zip-with using index
                    (let ((result nil)
                          (ks keys) (vs vals))
                      (while (and ks vs)
                        (setq result
                              (cons (cons (car ks) (car vs))
                                    result))
                        (setq ks (cdr ks) vs (cdr vs)))
                      ;; Format each pair
                      (mapconcat
                       (lambda (pair)
                         (format "%s=%s"
                                 (symbol-name (car pair))
                                 (if (numberp (cdr pair))
                                     (number-to-string (cdr pair))
                                   (cdr pair))))
                       (nreverse result)
                       "&")))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: mapconcat with recursive formatter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_mapconcat_recursive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Recursively format nested list as JSON-like string
    let form = r#"(progn
                    (fset 'neovm--test-to-json
                      (lambda (obj)
                        (cond
                         ((null obj) "null")
                         ((eq obj t) "true")
                         ((numberp obj) (number-to-string obj))
                         ((stringp obj) (format "\"%s\"" obj))
                         ((and (consp obj)
                               (symbolp (car obj)))
                          ;; alist entry: key-value pair
                          (format "\"%s\": %s"
                                  (symbol-name (car obj))
                                  (funcall 'neovm--test-to-json (cdr obj))))
                         ((listp obj)
                          ;; Check if it's an alist (all entries are cons with symbol car)
                          (if (and (consp (car obj))
                                   (symbolp (caar obj)))
                              (concat "{"
                                      (mapconcat
                                       'neovm--test-to-json
                                       obj ", ")
                                      "}")
                            (concat "["
                                    (mapconcat
                                     'neovm--test-to-json
                                     obj ", ")
                                    "]")))
                         (t "?"))))
                    (unwind-protect
                        (funcall 'neovm--test-to-json
                                 '((name . "Alice")
                                   (age . 30)
                                   (active . t)
                                   (scores . (95 87 92))))
                      (fmakunbound 'neovm--test-to-json)))"#;
    assert_oracle_parity(form);
}
