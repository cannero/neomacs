//! Advanced oracle parity tests for `string-search`: START-POS argument,
//! case sensitivity, empty string edge cases, boundary searches, repeated
//! searches to find all occurrences, combined with substring extraction,
//! and comparison with `string-match` behavior.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// START-POS: systematic exploration of offset behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_start_pos_systematic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Exhaustively test START-POS at every position in a string with
    // multiple occurrences, verifying each successive find.
    let form = r#"(let ((haystack "abcabcabc"))
  (list
    ;; Find "abc" starting from each valid position
    (string-search "abc" haystack 0)
    (string-search "abc" haystack 1)
    (string-search "abc" haystack 2)
    (string-search "abc" haystack 3)
    (string-search "abc" haystack 4)
    (string-search "abc" haystack 5)
    (string-search "abc" haystack 6)
    (string-search "abc" haystack 7)
    (string-search "abc" haystack 8)
    ;; START-POS exactly at string length
    (string-search "abc" haystack 9)
    ;; Single char search at every position
    (string-search "b" haystack 0)
    (string-search "b" haystack 1)
    (string-search "b" haystack 2)
    (string-search "b" haystack 4)
    (string-search "b" haystack 5)
    (string-search "b" haystack 8)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Case sensitivity behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_case_sensitivity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-search is always case-sensitive (unlike string-match which
    // respects case-fold-search). Verify this thoroughly.
    let form = r#"(list
  ;; Exact case matches
  (string-search "Hello" "Hello World")
  (string-search "hello" "Hello World")
  (string-search "HELLO" "Hello World")
  (string-search "hELLO" "Hello World")
  ;; Mixed case in haystack, searching for each variant
  (string-search "abc" "ABCabcAbcABC")
  (string-search "ABC" "ABCabcAbcABC")
  (string-search "Abc" "ABCabcAbcABC")
  (string-search "aBc" "ABCabcAbcABC")
  ;; Verify case-fold-search does NOT affect string-search
  (let ((case-fold-search t))
    (string-search "hello" "HELLO WORLD"))
  (let ((case-fold-search nil))
    (string-search "hello" "HELLO WORLD"))
  ;; Both bindings should return nil for string-search
  (let ((case-fold-search t))
    (string-search "HELLO" "HELLO WORLD")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Empty string needle and haystack edge cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_empty_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Empty needle in non-empty haystack: returns START-POS (default 0)
  (string-search "" "hello")
  (string-search "" "hello" 0)
  (string-search "" "hello" 3)
  (string-search "" "hello" 5)
  ;; Empty needle in empty haystack
  (string-search "" "")
  (string-search "" "" 0)
  ;; Non-empty needle in empty haystack
  (string-search "a" "")
  (string-search "abc" "")
  ;; Empty needle with various START-POS values
  (string-search "" "xy" 0)
  (string-search "" "xy" 1)
  (string-search "" "xy" 2)
  ;; Single character haystack
  (string-search "" "x" 0)
  (string-search "" "x" 1)
  (string-search "x" "x" 0)
  (string-search "x" "x" 1))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Repeated searches to find all occurrences (tokenizer pattern)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_find_all_occurrences() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use a while loop to find all positions of a substring, collecting
    // positions into a list, mimicking a simple tokenizer.
    let form = r#"(let ((find-all
         (lambda (needle haystack)
           "Return list of all positions where NEEDLE occurs in HAYSTACK."
           (let ((positions nil)
                 (start 0)
                 (nlen (length needle))
                 (pos nil))
             (while (setq pos (string-search needle haystack start))
               (setq positions (cons pos positions))
               (setq start (+ pos nlen)))
             (nreverse positions)))))
  (list
    ;; Multiple non-overlapping occurrences
    (funcall find-all "ab" "ababababab")
    ;; Single occurrence
    (funcall find-all "xyz" "abcxyzdef")
    ;; No occurrences
    (funcall find-all "zzz" "abcdef")
    ;; Needle at very start and very end
    (funcall find-all "xx" "xxmiddlexx")
    ;; Needle is the entire haystack
    (funcall find-all "hello" "hello")
    ;; Adjacent matches of single char
    (funcall find-all "a" "aaaa")
    ;; Real-world: find all comma positions for CSV parsing
    (funcall find-all "," "one,two,,four,five")
    ;; Longer needle with multiple matches
    (funcall find-all "the" "the cat on the mat ate the rat")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Combined with substring extraction for parsing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_with_substring_extraction() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use string-search + substring to implement split, field extraction,
    // and a simple URL parser.
    let form = r#"(let ((split-by
         (lambda (sep str)
           "Split STR by SEP, return list of parts."
           (let ((result nil)
                 (start 0)
                 (slen (length sep))
                 (pos nil))
             (while (setq pos (string-search sep str start))
               (setq result (cons (substring str start pos) result))
               (setq start (+ pos slen)))
             (setq result (cons (substring str start) result))
             (nreverse result)))))
  (list
    ;; Basic split
    (funcall split-by "," "a,b,c,d")
    ;; Split with multi-char separator
    (funcall split-by "::" "one::two::three")
    ;; Split with empty parts (consecutive separators)
    (funcall split-by "," "a,,b,,,c")
    ;; Split where sep is not present
    (funcall split-by ";" "no semicolons here")
    ;; Split empty string
    (funcall split-by "," "")
    ;; Parse URL-like structure: scheme://host:port/path
    (let* ((url "https://example.com:8080/api/v1/data")
           (scheme-end (string-search "://" url))
           (scheme (substring url 0 scheme-end))
           (after-scheme (substring url (+ scheme-end 3)))
           (path-start (string-search "/" after-scheme))
           (host-port (substring after-scheme 0 path-start))
           (path (substring after-scheme path-start))
           (colon-pos (string-search ":" host-port))
           (host (substring host-port 0 colon-pos))
           (port (substring host-port (1+ colon-pos))))
      (list scheme host port path))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Comparison with string-match behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_vs_string_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compare string-search (literal substring) with string-match (regexp).
    // They should agree on position for literal patterns, but string-match
    // treats special chars as regex.
    let form = r#"(list
  ;; Both should find "hello" at position 0
  (string-search "hello" "hello world")
  (string-match "hello" "hello world")
  ;; Both find "world" at position 6
  (string-search "world" "hello world")
  (string-match "world" "hello world")
  ;; string-search finds literal "a.b", string-match treats . as any char
  (string-search "a.b" "axb a.b")
  (string-match "a\\.b" "axb a.b")
  ;; string-search for "[a]" finds the literal brackets
  (string-search "[a]" "test [a] done")
  ;; string-match with regexp-quote to get literal behavior
  (string-match (regexp-quote "[a]") "test [a] done")
  ;; Both with START parameter
  (string-search "ab" "ababab" 2)
  (string-match "ab" "ababab" 2)
  ;; string-search returns nil, string-match returns nil
  (string-search "xyz" "abcdef")
  (string-match "xyz" "abcdef")
  ;; Verify positions match for simple literal searches at various offsets
  (let ((results nil)
        (text "the quick brown fox jumps over the lazy dog"))
    (dolist (word '("the" "fox" "dog" "over" "zzz"))
      (setq results
        (cons (list word
                    (string-search word text)
                    (string-match (regexp-quote word) text))
              results)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Boundary conditions: start/end of string, single-char strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_adv_boundary_conditions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Needle at the very start
  (string-search "abc" "abcdef")
  ;; Needle at the very end
  (string-search "def" "abcdef")
  ;; Needle is entire string
  (string-search "abcdef" "abcdef")
  ;; Needle one char longer than haystack
  (string-search "abcdefg" "abcdef")
  ;; Single char needle, single char haystack - match
  (string-search "a" "a")
  ;; Single char needle, single char haystack - no match
  (string-search "b" "a")
  ;; START-POS at last valid position
  (string-search "f" "abcdef" 5)
  (string-search "e" "abcdef" 5)
  ;; START-POS 0 with needle at position 0
  (string-search "a" "abcdef" 0)
  ;; Repeated chars: finding each subsequent one
  (let ((results nil)
        (s "aababcabcd")
        (pos 0))
    (while (and pos (< pos (length s)))
      (setq pos (string-search "abc" s pos))
      (when pos
        (setq results (cons pos results))
        (setq pos (1+ pos))))
    (nreverse results))
  ;; Overlapping potential: "aa" in "aaaa" (non-overlapping search)
  (let ((results nil)
        (s "aaaaa")
        (pos 0))
    (while (setq pos (string-search "aa" s pos))
      (setq results (cons pos results))
      (setq pos (+ pos 2)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
