//! Comprehensive oracle parity tests for regexp operations:
//! `string-match` vs `string-match-p`, `looking-at` vs `looking-at-p`,
//! `re-search-forward`/`re-search-backward` with BOUND, NOERROR, COUNT,
//! complex regex patterns, `match-string`/`match-beginning`/`match-end`
//! with subgroups, `replace-regexp-in-string` with function replacement,
//! `regexp-quote`, and back-references.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// string-match vs string-match-p: side effects on match-data
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_string_match_vs_match_p_side_effects() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-match sets match-data, string-match-p does NOT
    let form = r#"(progn
      ;; First set match-data via string-match
      (string-match "\\(foo\\)\\(bar\\)" "foobar")
      (let ((md1 (match-data)))
        ;; string-match-p should NOT alter match-data
        (string-match-p "xyz" "xyzabc")
        (let ((md2 (match-data)))
          ;; md1 and md2 should be equal since string-match-p doesn't change it
          (list 'md1 md1 'md2 md2 'equal (equal md1 md2)))))"#;
    assert_oracle_parity(form);

    // Verify string-match DOES overwrite match-data
    let form2 = r#"(progn
      (string-match "\\(aaa\\)" "aaa")
      (let ((first-md (match-data)))
        (string-match "\\(bbb\\)\\(ccc\\)" "bbbccc")
        (let ((second-md (match-data)))
          (list 'first first-md 'second second-md
                'different (not (equal first-md second-md))))))"#;
    assert_oracle_parity(form2);

    // string-match-p returns same match position as string-match
    let form3 = r#"(let ((s "hello world"))
      (list (string-match "world" s)
            (string-match-p "world" s)
            (string-match "o" s)
            (string-match-p "o" s)
            (string-match-p "xyz" s)
            (string-match "xyz" s)))"#;
    assert_oracle_parity(form3);
}

// ---------------------------------------------------------------------------
// looking-at vs looking-at-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_looking_at_vs_looking_at_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // looking-at sets match-data, looking-at-p does NOT
    let form = r#"(with-temp-buffer
      (insert "foobar baz quux")
      (goto-char (point-min))
      ;; looking-at sets match-data
      (let ((r1 (looking-at "\\(foo\\)\\(bar\\)")))
        (let ((md1 (match-data)))
          ;; Move to " baz"
          (goto-char 8)
          ;; looking-at-p should NOT change match-data
          (let ((r2 (looking-at-p "baz")))
            (let ((md2 (match-data)))
              (list 'r1 r1 'r2 r2
                    'md1 md1 'md2 md2
                    'match-data-preserved (equal md1 md2)))))))"#;
    assert_oracle_parity(form);

    // looking-at at various positions
    let form2 = r#"(with-temp-buffer
      (insert "abcdefghij")
      (goto-char (point-min))
      (list
       (looking-at "abc")
       (looking-at "bcd")
       (progn (goto-char 4) (looking-at "def"))
       (looking-at "^def")
       (progn (goto-char (point-min)) (looking-at "^abc"))))"#;
    assert_oracle_parity(form2);
}

// ---------------------------------------------------------------------------
// re-search-forward/backward with BOUND, NOERROR, COUNT
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_re_search_forward_all_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // BOUND parameter: limit search to specific position
    let form = r#"(with-temp-buffer
      (insert "aaa bbb aaa ccc aaa")
      (goto-char (point-min))
      ;; Search with bound at position 8 - should find first "aaa" only
      (let ((r1 (re-search-forward "aaa" 8 t)))
        (let ((p1 (point)))
          ;; Search again with same bound - should NOT find second "aaa"
          (let ((r2 (re-search-forward "aaa" 8 t)))
            (list r1 p1 r2 (point))))))"#;
    assert_oracle_parity(form);

    // NOERROR parameter: nil => error, t => return nil, other => move to limit
    let form2 = r#"(with-temp-buffer
      (insert "hello world")
      (goto-char (point-min))
      ;; NOERROR = t: return nil on failure, point unchanged
      (let ((r (re-search-forward "xyz" nil t)))
        (list r (point))))"#;
    assert_oracle_parity(form2);

    // NOERROR = non-nil non-t: move point to limit on failure
    let form3 = r#"(with-temp-buffer
      (insert "hello world")
      (goto-char (point-min))
      (let ((r (re-search-forward "xyz" nil 'move)))
        (list r (point) (= (point) (point-max)))))"#;
    assert_oracle_parity(form3);

    // COUNT parameter: find Nth occurrence
    let form4 = r#"(with-temp-buffer
      (insert "xx yy xx zz xx ww xx")
      (goto-char (point-min))
      (let ((r (re-search-forward "xx" nil t 3)))
        (list r (point))))"#;
    assert_oracle_parity(form4);
}

#[test]
fn oracle_prop_regexp_re_search_backward_all_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // re-search-backward with BOUND
    let form = r#"(with-temp-buffer
      (insert "aaa bbb aaa ccc aaa")
      (goto-char (point-max))
      ;; Backward search with bound at 10 - should not find first "aaa"
      (let ((r1 (re-search-backward "aaa" 10 t)))
        (let ((p1 (point)))
          ;; Backward search with bound at 1 - should find first "aaa"
          (let ((r2 (re-search-backward "aaa" 1 t)))
            (list r1 p1 r2 (point))))))"#;
    assert_oracle_parity(form);

    // re-search-backward with COUNT
    let form2 = r#"(with-temp-buffer
      (insert "ab cd ab ef ab gh ab")
      (goto-char (point-max))
      (let ((r (re-search-backward "ab" nil t 2)))
        (list r (point))))"#;
    assert_oracle_parity(form2);

    // NOERROR with backward search
    let form3 = r#"(with-temp-buffer
      (insert "hello world")
      (goto-char (point-max))
      (let ((r1 (re-search-backward "xyz" nil t))
            (p1 (point)))
        (goto-char (point-max))
        (let ((r2 (re-search-backward "xyz" nil 'move))
              (p2 (point)))
          (list r1 p1 r2 p2))))"#;
    assert_oracle_parity(form3);
}

// ---------------------------------------------------------------------------
// Complex regex patterns: grouping, alternation, repetition, char classes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_complex_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Alternation with grouping
    assert_oracle_parity(
        r#"(progn
          (string-match "\\(cat\\|dog\\|bird\\)" "I have a dog")
          (match-string 1 "I have a dog"))"#,
    );

    // Nested groups
    assert_oracle_parity(
        r#"(progn
          (string-match "\\(\\([0-9]+\\)-\\([0-9]+\\)\\)" "date: 2025-03")
          (list (match-string 0 "date: 2025-03")
                (match-string 1 "date: 2025-03")
                (match-string 2 "date: 2025-03")
                (match-string 3 "date: 2025-03")))"#,
    );

    // Character classes: [:alpha:], [:digit:], [:space:]
    assert_oracle_parity(
        r#"(progn
          (string-match "[[:digit:]]+" "abc 42 def")
          (match-string 0 "abc 42 def"))"#,
    );

    assert_oracle_parity(
        r#"(progn
          (string-match "[[:alpha:]]+" "123 hello 456")
          (match-string 0 "123 hello 456"))"#,
    );

    // Repetition: *, +, ?, counted
    assert_oracle_parity(
        r#"(list
          (string-match "ab*c" "ac")
          (string-match "ab*c" "abc")
          (string-match "ab*c" "abbbbc")
          (string-match "ab+c" "ac")
          (string-match "ab+c" "abc")
          (string-match "ab?c" "ac")
          (string-match "ab?c" "abc")
          (string-match "ab?c" "abbc"))"#,
    );

    // Shy groups \\(?: ... \\) don't capture
    assert_oracle_parity(
        r#"(progn
          (string-match "\\(?:foo\\|bar\\)-\\([0-9]+\\)" "bar-99")
          (list (match-string 0 "bar-99")
                (match-string 1 "bar-99")))"#,
    );
}

// ---------------------------------------------------------------------------
// match-string, match-beginning, match-end with subgroup numbers
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_match_accessors_subgroups() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Multiple groups with match-beginning/match-end
    let form = r#"(progn
      (string-match "\\([a-z]+\\)@\\([a-z]+\\)\\.\\([a-z]+\\)"
                     "user@example.com")
      (list
       ;; Group 0: whole match
       (match-beginning 0) (match-end 0)
       (match-string 0 "user@example.com")
       ;; Group 1: user
       (match-beginning 1) (match-end 1)
       (match-string 1 "user@example.com")
       ;; Group 2: domain
       (match-beginning 2) (match-end 2)
       (match-string 2 "user@example.com")
       ;; Group 3: tld
       (match-beginning 3) (match-end 3)
       (match-string 3 "user@example.com")))"#;
    assert_oracle_parity(form);

    // Unmatched optional group returns nil
    let form2 = r#"(progn
      (string-match "\\(foo\\)\\(-\\([a-z]+\\)\\)?" "foo")
      (list (match-string 1 "foo")
            (match-string 2 "foo")
            (match-string 3 "foo")
            (match-beginning 2)
            (match-end 2)))"#;
    assert_oracle_parity(form2);

    // match-data as a flat list of integers
    let form3 = r#"(progn
      (string-match "\\(ab\\)\\(cd\\)\\(ef\\)" "xabcdefx")
      (match-data))"#;
    assert_oracle_parity(form3);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string with function replacement
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_replace_with_function() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Function replacement: receives matched string
    assert_oracle_parity(
        r#"(replace-regexp-in-string
           "[0-9]+"
           (lambda (m) (number-to-string (* 3 (string-to-number m))))
           "a1 b22 c333")"#,
    );

    // Function with upcase
    assert_oracle_parity(
        r#"(replace-regexp-in-string "\\b[a-z]" #'upcase "hello world foo")"#,
    );

    // Function that accesses match-data to get subgroups
    assert_oracle_parity(
        r#"(replace-regexp-in-string
           "\\([a-z]+\\)=\\([0-9]+\\)"
           (lambda (m)
             (format "%s:%s" (upcase (match-string 1 m)) (match-string 2 m)))
           "foo=1 bar=2 baz=3")"#,
    );

    // Function replacement with counter (closure)
    assert_oracle_parity(
        r#"(let ((n 0))
           (replace-regexp-in-string
            "X"
            (lambda (_m) (setq n (1+ n)) (number-to-string n))
            "X and X and X"))"#,
    );
}

// ---------------------------------------------------------------------------
// regexp-quote with special chars
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_quote_special_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // regexp-quote escapes all special regex characters
    assert_oracle_parity(
        r#"(list
          (regexp-quote "hello")
          (regexp-quote "foo.bar")
          (regexp-quote "a*b+c?")
          (regexp-quote "[abc]")
          (regexp-quote "\\(group\\)")
          (regexp-quote "^start$end")
          (regexp-quote "a|b")
          (regexp-quote "price: $5.00"))"#,
    );

    // Use regexp-quote to search for literal special chars
    assert_oracle_parity(
        r#"(let ((needle "foo.bar"))
           (string-match (regexp-quote needle) "test foo.bar test"))"#,
    );

    // regexp-quote + concat for anchored literal search
    assert_oracle_parity(
        r#"(let ((literal "a+b"))
           (list
            (string-match (concat "^" (regexp-quote literal)) "a+b stuff")
            (string-match (concat "^" (regexp-quote literal)) "aab stuff")))"#,
    );
}

// ---------------------------------------------------------------------------
// Back-references in patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_back_references() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // \1 back-reference: match repeated word
    assert_oracle_parity(
        r#"(progn
          (string-match "\\([a-z]+\\) \\1" "the the cat")
          (list (match-string 0 "the the cat")
                (match-string 1 "the the cat")))"#,
    );

    // Back-reference in replace-regexp-in-string
    assert_oracle_parity(
        r#"(replace-regexp-in-string
           "\\([a-z]+\\)-\\([0-9]+\\)"
           "\\2_\\1"
           "foo-123 bar-456 baz-789")"#,
    );

    // Back-reference: detect palindrome-like pattern (aba)
    assert_oracle_parity(
        r#"(list
          (string-match "\\(..\\)..\\1" "abcdab")
          (when (string-match "\\(..\\)..\\1" "abcdab")
            (match-string 0 "abcdab")))"#,
    );

    // Multiple back-references
    assert_oracle_parity(
        r#"(progn
          (string-match "\\([a-z]\\)\\([a-z]\\)\\2\\1" "abba xyzzy")
          (list (match-string 0 "abba xyzzy")
                (match-string 1 "abba xyzzy")
                (match-string 2 "abba xyzzy")))"#,
    );
}

// ---------------------------------------------------------------------------
// Complex: iterative search collecting all matches with positions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_iterative_search_collecting() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Collect all matches of a pattern with their positions in a buffer
    let form = r#"(with-temp-buffer
      (insert "The quick brown fox jumps over the lazy fox")
      (goto-char (point-min))
      (let ((matches nil))
        (while (re-search-forward "\\b\\([a-z]+\\)\\b" nil t)
          (setq matches
                (cons (list (match-beginning 0)
                            (match-end 0)
                            (match-string 1))
                      matches)))
        (nreverse matches)))"#;
    assert_oracle_parity(form);

    // Collect all matches of a pattern in a string using string-match + START
    let form2 = r#"(let ((s "aa123bb456cc789dd")
                         (pos 0)
                         (nums nil))
      (while (string-match "[0-9]+" s pos)
        (setq nums (cons (list (match-beginning 0) (match-string 0 s)) nums))
        (setq pos (match-end 0)))
      (nreverse nums))"#;
    assert_oracle_parity(form2);
}

// ---------------------------------------------------------------------------
// Complex: regex-based tokenizer
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_tokenizer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a simple tokenizer that classifies tokens by regex
    let form = r#"(let ((input "let x = 42 + y * 3.14")
                       (pos 0)
                       (tokens nil)
                       (patterns '(("[ \t]+" . ws)
                                   ("[a-zA-Z_][a-zA-Z0-9_]*" . ident)
                                   ("[0-9]+\\(?:\\.[0-9]+\\)?" . number)
                                   ("[=+*]" . operator))))
      (while (< pos (length input))
        (let ((matched nil))
          (dolist (pat patterns)
            (unless matched
              (when (and (string-match (concat "\\`" (car pat))
                                       (substring input pos))
                         (= (match-beginning 0) 0))
                (let ((tok (match-string 0 (substring input pos))))
                  (unless (eq (cdr pat) 'ws)
                    (setq tokens (cons (list (cdr pat) tok) tokens)))
                  (setq pos (+ pos (length tok)))
                  (setq matched t)))))
          (unless matched
            (setq pos (1+ pos)))))
      (nreverse tokens))"#;
    assert_oracle_parity(form);
}
