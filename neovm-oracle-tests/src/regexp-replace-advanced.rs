//! Advanced oracle parity tests for `replace-regexp-in-string` and
//! `replace-match` with FIXEDCASE, LITERAL, START, SUBEXP, and
//! function-as-replacement parameters.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// replace-regexp-in-string: FIXEDCASE param (nil vs t)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_fixedcase_nil_preserves_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // With FIXEDCASE=nil (default), Emacs tries to preserve the case
    // pattern of the matched text in the replacement.
    let form = r#"(list
      (replace-regexp-in-string "hello" "world" "Hello there, HELLO again, hello end" nil)
      (replace-regexp-in-string "foo" "bar" "Foo FOO foo fOO" nil)
      (replace-regexp-in-string "cat" "dog" "Cat CAT cat CaT" nil))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_fixedcase_t_exact_replacement() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // With FIXEDCASE=t, the replacement is inserted exactly as given,
    // no case transformation.
    let form = r#"(list
      (replace-regexp-in-string "hello" "world" "Hello there, HELLO again, hello end" t)
      (replace-regexp-in-string "foo" "xYz" "Foo FOO foo" t))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string: LITERAL param
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_literal_backslash_handling() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // LITERAL=t means backslashes in replacement are not special.
    // LITERAL=nil means \& refers to whole match, \1 to group 1, etc.
    let form = r#"(list
      ;; LITERAL=nil: \& expands to the match
      (replace-regexp-in-string "\\([0-9]+\\)" "[\\&]" "val=42 x=99" nil nil)
      ;; LITERAL=t: \& is kept literally
      (replace-regexp-in-string "\\([0-9]+\\)" "[\\&]" "val=42 x=99" nil t)
      ;; LITERAL=nil: \1 expands to group 1
      (replace-regexp-in-string "\\([a-z]+\\)=\\([0-9]+\\)" "\\2->\\1" "foo=10 bar=20" nil nil)
      ;; LITERAL=t: \1 kept literally
      (replace-regexp-in-string "\\([a-z]+\\)=\\([0-9]+\\)" "\\2->\\1" "foo=10 bar=20" nil t))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string: START param
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_start_offset() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // START param (6th arg) specifies character position to begin matching.
    // Characters before START are included in result unchanged.
    let form = r#"(list
      ;; Replace digits only after position 5
      (replace-regexp-in-string "[0-9]+" "NUM" "12 ab 34 cd 56" nil nil 5)
      ;; Replace from position 0 (all matches)
      (replace-regexp-in-string "[0-9]+" "NUM" "12 ab 34 cd 56" nil nil 0)
      ;; START beyond all matches
      (replace-regexp-in-string "[0-9]+" "NUM" "12 ab 34 cd 56" nil nil 14)
      ;; START in the middle of a potential match
      (replace-regexp-in-string "abcd" "XXXX" "abcd-abcd-abcd" nil nil 3))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string: REP as function (lambda taking match)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_function_replacement() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // When REP is a function, it receives the matched string and returns
    // the replacement string.
    let form = r#"(list
      ;; Double each number
      (replace-regexp-in-string
        "[0-9]+"
        (lambda (m) (number-to-string (* 2 (string-to-number m))))
        "a=5 b=10 c=25")
      ;; Upcase each word
      (replace-regexp-in-string
        "[a-z]+"
        (lambda (m) (upcase m))
        "hello world foo")
      ;; Wrap each match in brackets with length
      (replace-regexp-in-string
        "[A-Z][a-z]+"
        (lambda (m) (format "[%s:%d]" m (length m)))
        "Alice met Bob and Charlie"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-match with all 4 params: FIXEDCASE, LITERAL, STRING, SUBEXP
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_match_all_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // replace-match on a string with SUBEXP: only replace the captured group
    let form = r#"(let ((s "prefix:VALUE:suffix"))
      (string-match "prefix:\\([A-Z]+\\):suffix" s)
      (list
        ;; Replace entire match (subexp=0 or nil)
        (replace-match "REPLACED" t nil s)
        ;; Replace only group 1 (subexp=1), FIXEDCASE=t, LITERAL=t
        (replace-match "newval" t t s 1)
        ;; Replace group 1 with FIXEDCASE=nil (case adaptation)
        (replace-match "newval" nil nil s 1)
        ;; Verify match-string still works on original
        (match-string 1 s)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: multi-pass regexp transformation pipeline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_multi_pass_transform_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Apply a chain of regexp transformations to normalize messy text:
    // 1. Collapse multiple spaces to single space
    // 2. Trim leading/trailing whitespace per line
    // 3. Convert snake_case identifiers to camelCase
    // 4. Wrap numbers in <num>...</num> tags
    let form = r#"(let ((text "  foo_bar  baz_quux  42  hello_world  7  "))
      (let* ((step1 (replace-regexp-in-string "  +" " " text))
             (step2 (replace-regexp-in-string "\\` \\| \\'" "" step1))
             (step3 (replace-regexp-in-string
                      "_\\([a-z]\\)"
                      (lambda (m)
                        (upcase (substring m 1)))
                      step2))
             (step4 (replace-regexp-in-string
                      "\\b[0-9]+\\b"
                      (lambda (m) (concat "<num>" m "</num>"))
                      step3)))
        (list step1 step2 step3 step4)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// regexp-quote for escaping special chars in patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_quote_in_search_replace() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use regexp-quote to safely build patterns from user input containing
    // special regex characters, then use them in replace-regexp-in-string.
    let form = r#"(let ((needles '("foo.bar" "a*b" "[test]" "x+y" "a\\b" "(hi)"))
                        (results nil))
      (dolist (needle needles)
        (let* ((input (concat "before " needle " after " needle " end"))
               (pattern (regexp-quote needle))
               (replaced (replace-regexp-in-string pattern "REPLACED" input)))
          (setq results (cons replaced results))))
      (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}
