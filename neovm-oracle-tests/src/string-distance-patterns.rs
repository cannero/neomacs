//! Advanced oracle parity tests for `string-distance` (Levenshtein) patterns:
//! identity distance, single-operation distances, symmetry verification,
//! triangle inequality across many strings, sorting by distance, fuzzy
//! matching with scoring, and spell-checker simulation.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity_with_bootstrap;

// ---------------------------------------------------------------------------
// Identity, single operation (insert/delete/substitute), and boundary cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_single_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify that identical strings have distance 0, single insert/delete/sub
    // always yields distance 1, and boundary cases (empty, single char) work.
    let form = r#"
(let ((results nil))
  ;; Identical strings => distance 0
  (dolist (s '("" "a" "hello" "abracadabra" "the quick brown fox"))
    (setq results (cons (list 'ident s (string-distance s s)) results)))

  ;; Single insertion => distance 1
  (let ((pairs '(("" "x")
                 ("abc" "abcd")
                 ("abc" "xabc")
                 ("abc" "abxc")
                 ("hello" "helloo"))))
    (dolist (p pairs)
      (let ((a (car p)) (b (cadr p)))
        (setq results (cons (list 'insert a b (string-distance a b)) results)))))

  ;; Single deletion => distance 1 (reverse of insertion)
  (let ((pairs '(("x" "")
                 ("abcd" "abc")
                 ("xabc" "abc")
                 ("abxc" "abc"))))
    (dolist (p pairs)
      (let ((a (car p)) (b (cadr p)))
        (setq results (cons (list 'delete a b (string-distance a b)) results)))))

  ;; Single substitution => distance 1
  (let ((pairs '(("a" "b")
                 ("cat" "bat")
                 ("cat" "cot")
                 ("cat" "cas")
                 ("hello" "hallo"))))
    (dolist (p pairs)
      (let ((a (car p)) (b (cadr p)))
        (setq results (cons (list 'subst a b (string-distance a b)) results)))))

  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Symmetry: d(a,b) = d(b,a) for many string pairs
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_symmetry_exhaustive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify symmetry across many pairs including mixed-length, repeated chars,
    // common prefixes/suffixes, and completely disjoint strings.
    let form = r#"
(let ((strings '("" "a" "ab" "abc" "abcd" "dcba" "aaaa" "bbbb"
                 "kitten" "sitting" "sunday" "saturday"
                 "algorithm" "altruistic" "pneumonoultramicroscopicsilicovolcanoconiosis"
                 "supercalifragilisticexpialidocious"))
      (all-symmetric t)
      (checked 0)
      (counterexamples nil))
  (let ((i 0))
    (dolist (a strings)
      (let ((j 0))
        (dolist (b strings)
          (when (>= j i)
            (let ((d-ab (string-distance a b))
                  (d-ba (string-distance b a)))
              (setq checked (1+ checked))
              (unless (= d-ab d-ba)
                (setq all-symmetric nil)
                (setq counterexamples
                      (cons (list a b d-ab d-ba) counterexamples)))))
          (setq j (1+ j))))
      (setq i (1+ i))))
  (list 'symmetric all-symmetric
        'pairs-checked checked
        'counterexamples counterexamples))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Triangle inequality: d(a,c) <= d(a,b) + d(b,c)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_triangle_inequality_broad() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Exhaustively verify the metric triangle inequality across many triples,
    // including edge cases with empty strings and repeated characters.
    let form = r#"
(let ((strings '("" "a" "ab" "ba" "abc" "xyz" "aaa" "bbb"
                 "kitten" "sitting" "hello" "world" "test"))
      (violations 0)
      (total 0)
      (max-slack 0))
  (dolist (a strings)
    (dolist (b strings)
      (dolist (c strings)
        (let* ((d-ab (string-distance a b))
               (d-bc (string-distance b c))
               (d-ac (string-distance a c))
               (slack (- (+ d-ab d-bc) d-ac)))
          (setq total (1+ total))
          (when (< slack 0)
            (setq violations (1+ violations)))
          (when (> slack max-slack)
            (setq max-slack slack))))))
  (list 'violations violations
        'total total
        'all-valid (= violations 0)
        'max-slack max-slack))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Sorting candidates by distance from a target
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_sort_by_distance() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort a list of words by their Levenshtein distance to a target word,
    // breaking ties alphabetically. Test with multiple target words.
    let form = r#"
(progn
  (fset 'neovm--sdp-sort-by-distance
    (lambda (target candidates)
      (let ((scored (mapcar (lambda (c)
                              (cons (string-distance target c) c))
                            candidates)))
        (setq scored (sort scored
                          (lambda (a b)
                            (or (< (car a) (car b))
                                (and (= (car a) (car b))
                                     (string< (cdr a) (cdr b)))))))
        scored)))

  (unwind-protect
      (let ((words '("apple" "apply" "ape" "maple" "ample"
                     "application" "appeal" "applet" "apricot"
                     "banana" "mango" "grape" "pineapple")))
        (list
          ;; Sort by distance to "apple"
          (funcall 'neovm--sdp-sort-by-distance "apple" words)
          ;; Sort by distance to "banana"
          (funcall 'neovm--sdp-sort-by-distance "banana" words)
          ;; Sort by distance to "app" (short query)
          (funcall 'neovm--sdp-sort-by-distance "app" words)
          ;; Sort by distance to "" (empty - should rank by length)
          (funcall 'neovm--sdp-sort-by-distance "" words)))
    (fmakunbound 'neovm--sdp-sort-by-distance)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Fuzzy matching with multi-criteria scoring
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_fuzzy_match_scoring() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Multi-criteria fuzzy match scoring: combines edit distance, prefix bonus,
    // length penalty, and substring bonus. Returns top-N ranked results.
    let form = r#"
(progn
  (fset 'neovm--sdp-has-prefix
    (lambda (str prefix)
      (and (>= (length str) (length prefix))
           (string= (substring str 0 (length prefix)) prefix))))

  (fset 'neovm--sdp-contains
    (lambda (haystack needle)
      (let ((found nil) (nlen (length needle)) (hlen (length haystack)))
        (when (<= nlen hlen)
          (let ((i 0))
            (while (and (not found) (<= (+ i nlen) hlen))
              (when (string= (substring haystack i (+ i nlen)) needle)
                (setq found t))
              (setq i (1+ i)))))
        found)))

  (fset 'neovm--sdp-fuzzy-score
    (lambda (query candidate)
      (let* ((dist (string-distance query candidate))
             (maxlen (max (length query) (length candidate) 1))
             ;; Base score: inverse of normalized distance (0-1000)
             (base-score (- 1000 (/ (* 1000 dist) maxlen)))
             ;; Prefix bonus: +200 if candidate starts with query
             (prefix-bonus (if (funcall 'neovm--sdp-has-prefix candidate query)
                               200 0))
             ;; Substring bonus: +100 if query is a substring
             (substr-bonus (if (and (> (length query) 0)
                                    (funcall 'neovm--sdp-contains candidate query))
                               100 0))
             ;; Length penalty: -5 per extra char beyond query length
             (len-penalty (* 5 (max 0 (- (length candidate) (length query))))))
        (+ base-score prefix-bonus substr-bonus (- len-penalty)))))

  (fset 'neovm--sdp-fuzzy-top-n
    (lambda (query candidates n)
      (let* ((scored (mapcar (lambda (c)
                               (cons (funcall 'neovm--sdp-fuzzy-score query c) c))
                             candidates))
             (sorted (sort scored (lambda (a b) (> (car a) (car b))))))
        ;; Take top N
        (let ((result nil) (count 0))
          (while (and sorted (< count n))
            (setq result (cons (car sorted) result))
            (setq sorted (cdr sorted))
            (setq count (1+ count)))
          (nreverse result)))))

  (unwind-protect
      (let ((commands '("find-file" "find-file-other-window" "find-file-read-only"
                        "find-tag" "fill-paragraph" "fill-region"
                        "forward-char" "forward-word" "forward-line"
                        "fundamental-mode" "font-lock-mode"
                        "flycheck-mode" "flymake-mode")))
        (list
          ;; Top 5 matches for "find"
          (funcall 'neovm--sdp-fuzzy-top-n "find" commands 5)
          ;; Top 5 for "fill"
          (funcall 'neovm--sdp-fuzzy-top-n "fill" commands 5)
          ;; Top 5 for "for"
          (funcall 'neovm--sdp-fuzzy-top-n "for" commands 5)
          ;; Top 3 for "fly"
          (funcall 'neovm--sdp-fuzzy-top-n "fly" commands 3)
          ;; Top 5 for "f" (very short query)
          (funcall 'neovm--sdp-fuzzy-top-n "f" commands 5)))
    (fmakunbound 'neovm--sdp-has-prefix)
    (fmakunbound 'neovm--sdp-contains)
    (fmakunbound 'neovm--sdp-fuzzy-score)
    (fmakunbound 'neovm--sdp-fuzzy-top-n)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Spell checker simulation using string-distance
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_spell_checker() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a spell checker that suggests corrections from a dictionary.
    // For each misspelled word, find all dictionary words within distance 2,
    // rank them, and return the best suggestion. Also handle already-correct words.
    let form = r#"
(progn
  (fset 'neovm--sdp-spell-suggest
    (lambda (word dictionary max-dist)
      (let ((suggestions nil))
        (dolist (dict-word dictionary)
          (let ((d (string-distance word dict-word)))
            (when (<= d max-dist)
              (setq suggestions (cons (cons d dict-word) suggestions)))))
        ;; Sort by distance then alphabetically
        (setq suggestions
              (sort suggestions
                    (lambda (a b)
                      (or (< (car a) (car b))
                          (and (= (car a) (car b))
                               (string< (cdr a) (cdr b)))))))
        suggestions)))

  (fset 'neovm--sdp-spell-check
    (lambda (words dictionary)
      (mapcar (lambda (word)
                (let ((suggestions (funcall 'neovm--sdp-spell-suggest
                                            word dictionary 2)))
                  (cond
                    ;; Exact match found (distance 0)
                    ((and suggestions (= (car (car suggestions)) 0))
                     (list word 'correct))
                    ;; Has suggestions
                    (suggestions
                     (list word 'misspelled
                           (mapcar 'cdr (let ((top nil) (n 0))
                                          (while (and suggestions (< n 3))
                                            (setq top (cons (car suggestions) top))
                                            (setq suggestions (cdr suggestions))
                                            (setq n (1+ n)))
                                          (nreverse top)))))
                    ;; No suggestions
                    (t (list word 'unknown)))))
              words)))

  (unwind-protect
      (let ((dictionary '("the" "their" "there" "then" "these" "those"
                          "this" "that" "than" "them"
                          "and" "any" "all" "are" "also"
                          "be" "been" "but" "both" "by"
                          "can" "could" "come" "car" "cat"
                          "do" "did" "does" "down"
                          "each" "even" "every"
                          "for" "from" "find" "first"
                          "get" "got" "good" "great"
                          "have" "has" "had" "help" "here")))
        (list
          ;; Check correct words
          (funcall 'neovm--sdp-spell-check
                   '("the" "and" "for" "have") dictionary)
          ;; Check misspelled words
          (funcall 'neovm--sdp-spell-check
                   '("teh" "adn" "fro" "hav") dictionary)
          ;; Check words with no close match
          (funcall 'neovm--sdp-spell-check
                   '("xyz" "qqq" "zzz") dictionary)
          ;; Mixed correct and misspelled
          (funcall 'neovm--sdp-spell-check
                   '("the" "thn" "and" "anf" "cat" "cta") dictionary)))
    (fmakunbound 'neovm--sdp-spell-suggest)
    (fmakunbound 'neovm--sdp-spell-check)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Byte-mode vs char-mode distance comparison with ASCII strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_distance_byte_vs_char_mode() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // For pure ASCII strings, byte mode (3rd arg t) should match char mode.
    // Verify this property and also test that distances combine correctly
    // in a pipeline that computes edit distance matrices.
    let form = r#"
(let ((strings '("" "a" "ab" "abc" "hello" "world" "test" "testing"
                 "algorithm" "logarithm" "kitten" "sitting"))
      (all-match t)
      (matrix nil))
  ;; Verify byte=char for ASCII strings
  (dolist (a strings)
    (dolist (b strings)
      (let ((d-char (string-distance a b))
            (d-byte (string-distance a b t)))
        (unless (= d-char d-byte)
          (setq all-match nil)))))

  ;; Build a distance matrix for a subset
  (let ((subset '("cat" "bat" "hat" "car" "bar" "cab")))
    (setq matrix
          (mapcar (lambda (a)
                    (cons a (mapcar (lambda (b)
                                     (string-distance a b))
                                   subset)))
                  subset)))

  ;; Find the pair with maximum distance and minimum non-zero distance
  (let ((max-dist 0) (max-pair nil)
        (min-dist most-positive-fixnum) (min-pair nil))
    (let ((subset '("cat" "bat" "hat" "car" "bar" "cab")))
      (dolist (a subset)
        (dolist (b subset)
          (unless (string= a b)
            (let ((d (string-distance a b)))
              (when (> d max-dist)
                (setq max-dist d max-pair (list a b)))
              (when (< d min-dist)
                (setq min-dist d min-pair (list a b))))))))
    (list 'ascii-byte-eq-char all-match
          'matrix matrix
          'max-pair (list max-dist max-pair)
          'min-pair (list min-dist min-pair))))
"#;
    assert_oracle_parity_with_bootstrap(form);
}
