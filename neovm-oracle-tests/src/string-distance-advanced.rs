//! Advanced oracle parity tests for `string-distance`, `string-version-lessp`,
//! `string-lessp`, and `compare-strings`.
//!
//! Tests edge cases, fuzzy matching scoring, string similarity ranking,
//! and diff-like algorithms built on string-distance.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity_with_bootstrap;

// ---------------------------------------------------------------------------
// string-distance edge cases and symmetry
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_edge_cases_and_symmetry() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Edge cases: empty strings, single char, identical, completely different.
    // Also verify symmetry: distance(a,b) == distance(b,a).
    let form = r#"
(let ((pairs '(("" "")
               ("" "a")
               ("a" "")
               ("a" "a")
               ("a" "b")
               ("abc" "abc")
               ("abc" "xyz")
               ("" "abcdef")
               ("abcdef" "")
               ("ab" "ba")
               ("kitten" "sitting")
               ("saturday" "sunday"))))
  (mapcar (lambda (pair)
            (let ((a (car pair))
                  (b (cadr pair)))
              (list (string-distance a b)
                    (string-distance b a)
                    (= (string-distance a b)
                       (string-distance b a)))))
          pairs))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-distance with byte-length mode
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_byte_mode() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Third argument t means compare by bytes instead of characters.
    // For pure ASCII strings the results are the same; for multibyte they differ.
    let form = r#"
(list
  ;; ASCII: byte mode same as char mode
  (= (string-distance "hello" "hallo")
     (string-distance "hello" "hallo" t))
  ;; single-char insertions
  (string-distance "abc" "abcd" t)
  (string-distance "abcd" "abc" t)
  ;; substitutions
  (string-distance "cat" "hat" t)
  ;; empty
  (string-distance "" "" t)
  (string-distance "" "xyz" t)
  ;; longer strings
  (string-distance "algorithm" "altruistic" t)
  (string-distance "intention" "execution" t))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-version-lessp comprehensive version comparisons
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_version_lessp_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Systematic testing of version string comparison semantics.
    let form = r#"
(let ((pairs '(("1.0"    "1.1"     t)
               ("1.1"    "1.0"     nil)
               ("1.9"    "1.10"    t)
               ("1.10"   "1.9"     nil)
               ("2.0"    "10.0"    t)
               ("10.0"   "2.0"     nil)
               ("1.0.0"  "1.0.1"   t)
               ("1.0.1"  "1.0.0"   nil)
               ("0.9.9"  "1.0.0"   t)
               ("1.0.0"  "1.0.0"   nil)
               ("foo2"   "foo10"   t)
               ("foo10"  "foo2"    nil)
               ("bar"    "bar1"    t)
               ("bar1"   "bar"     nil)
               ("a1b2"   "a1b10"   t)
               ("a1b10"  "a1b2"    nil)
               (""       ""        nil)
               (""       "a"       t)
               ("a"      ""        nil)
               ("001"    "1"       nil)
               ("1"      "001"     nil))))
  (mapcar (lambda (entry)
            (let ((a (nth 0 entry))
                  (b (nth 1 entry))
                  (expected (nth 2 entry)))
              (list a b
                    (if (string-version-lessp a b) t nil)
                    (eq (if (string-version-lessp a b) t nil) expected))))
          pairs))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-lessp vs string< behavior and compare-strings interaction
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_comparison_functions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-lessp and string< should be identical (string< is an alias).
    // compare-strings provides richer output (position of first difference).
    let form = r#"
(let ((pairs '(("abc" "abd")
               ("abd" "abc")
               ("abc" "abc")
               ("abc" "abcd")
               ("abcd" "abc")
               ("" "a")
               ("a" "")
               ("" "")
               ("ABC" "abc")
               ("abc" "ABC")
               ("a" "z")
               ("z" "a"))))
  (mapcar (lambda (pair)
            (let ((a (car pair))
                  (b (cadr pair)))
              (list
                ;; string-lessp and string< agree
                (eq (string-lessp a b) (string< a b))
                ;; string-lessp result
                (if (string-lessp a b) t nil)
                ;; compare-strings result (case-sensitive)
                (compare-strings a nil nil b nil nil)
                ;; compare-strings case-insensitive
                (compare-strings a nil nil b nil nil t))))
          pairs))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: fuzzy string matching using string-distance scoring
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fuzzy_string_matching() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement a fuzzy matcher that scores candidates against a query.
    // Score = 1.0 - (distance / max-length). Return sorted candidates.
    let form = r#"
(progn
  (fset 'neovm--sd-fuzzy-score
    (lambda (query candidate)
      (let* ((dist (string-distance query candidate))
             (maxlen (max (length query) (length candidate) 1)))
        ;; Return score * 1000 as integer to avoid float comparison issues
        (- 1000 (/ (* 1000 dist) maxlen)))))

  (fset 'neovm--sd-fuzzy-match
    (lambda (query candidates)
      (let ((scored (mapcar (lambda (c)
                              (cons (funcall 'neovm--sd-fuzzy-score query c) c))
                            candidates)))
        ;; Sort by score descending
        (setq scored (sort scored (lambda (a b) (> (car a) (car b)))))
        ;; Return (score . candidate) pairs
        scored)))

  (unwind-protect
      (let ((candidates '("function" "fun" "fundamental" "funeral"
                           "fusion" "fuzzy" "fund" "funny")))
        (funcall 'neovm--sd-fuzzy-match "fun" candidates))
    (fmakunbound 'neovm--sd-fuzzy-score)
    (fmakunbound 'neovm--sd-fuzzy-match)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: string similarity ranking for auto-complete
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_autocomplete_ranking() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate an auto-complete ranking system that combines prefix matching,
    // string-distance, and string-version-lessp for tie-breaking.
    let form = r#"
(progn
  (fset 'neovm--sd-rank-candidates
    (lambda (prefix candidates)
      (let* ((plen (length prefix))
             ;; Phase 1: filter by prefix match
             (prefix-matches
              (let ((result nil))
                (dolist (c candidates)
                  (when (and (>= (length c) plen)
                             (string-equal (substring c 0 plen) prefix))
                    (setq result (cons c result))))
                (nreverse result)))
             ;; Phase 2: rank remaining by distance to full prefix
             (non-prefix
              (let ((result nil))
                (dolist (c candidates)
                  (unless (and (>= (length c) plen)
                               (string-equal (substring c 0 plen) prefix))
                    (setq result (cons c result))))
                (nreverse result)))
             ;; Sort prefix matches by length (shorter = better)
             (sorted-prefix
              (sort prefix-matches
                    (lambda (a b) (< (length a) (length b)))))
             ;; Sort non-prefix by edit distance
             (sorted-non
              (sort non-prefix
                    (lambda (a b)
                      (< (string-distance prefix a)
                         (string-distance prefix b))))))
        (list sorted-prefix sorted-non))))

  (unwind-protect
      (funcall 'neovm--sd-rank-candidates "buf"
               '("buffer" "buffer-name" "buffer-list" "bury-buffer"
                 "buf" "build" "bulk" "bufferp" "bug-report"))
    (fmakunbound 'neovm--sd-rank-candidates)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: diff-like algorithm using string-distance on split lines
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_line_diff_algorithm() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Given two "documents" (lists of lines), compute a simple diff.
    // For each line in doc-b, find the closest line in doc-a using
    // string-distance. Lines with distance 0 are "unchanged", small
    // distance is "modified", large distance or no match is "added".
    let form = r#"
(progn
  (fset 'neovm--sd-find-closest
    (lambda (line lines)
      (let ((best-dist most-positive-fixnum)
            (best-line nil))
        (dolist (l lines)
          (let ((d (string-distance line l)))
            (when (< d best-dist)
              (setq best-dist d best-line l))))
        (cons best-dist best-line))))

  (fset 'neovm--sd-diff-docs
    (lambda (doc-a doc-b)
      (let ((result nil))
        (dolist (line-b doc-b)
          (let* ((closest (funcall 'neovm--sd-find-closest line-b doc-a))
                 (dist (car closest))
                 (matched (cdr closest))
                 (status (cond
                           ((= dist 0) 'unchanged)
                           ((< dist (/ (max (length line-b)
                                            (length (or matched "")))
                                       2))
                            'modified)
                           (t 'added))))
            (setq result (cons (list status line-b dist) result))))
        (nreverse result))))

  (unwind-protect
      (let ((doc-a '("(defun hello ()" "  (message \"hello\")" "  (newline))"))
            (doc-b '("(defun hello (name)" "  (message \"hello %s\" name)" "  (newline))" "(provide 'hello)")))
        (funcall 'neovm--sd-diff-docs doc-a doc-b))
    (fmakunbound 'neovm--sd-find-closest)
    (fmakunbound 'neovm--sd-diff-docs)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: triangle inequality property of string-distance
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_triangle_inequality() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // The Levenshtein distance satisfies the triangle inequality:
    // distance(a,c) <= distance(a,b) + distance(b,c) for all strings a,b,c.
    // Verify this property across many triples.
    let form = r#"
(let ((strings '("" "a" "ab" "abc" "xyz" "kitten" "sitting" "hello" "world")))
  (let ((violations 0)
        (total 0)
        (examples nil))
    (dolist (a strings)
      (dolist (b strings)
        (dolist (c strings)
          (let ((ab (string-distance a b))
                (bc (string-distance b c))
                (ac (string-distance a c)))
            (setq total (1+ total))
            (unless (<= ac (+ ab bc))
              (setq violations (1+ violations))
              (setq examples (cons (list a b c ab bc ac) examples)))))))
    (list violations total (null examples))))
"#;
    assert_oracle_parity_with_bootstrap(form);
}
