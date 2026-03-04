//! Comprehensive oracle parity tests for string manipulation functions:
//! substring with all parameter combinations, concat with mixed types,
//! string-replace, replace-regexp-in-string with all params,
//! split-string with SEPARATORS/OMIT-NULLS/TRIM, string-join,
//! string-trim variants with custom TRIM-CHARS, string-chop-newline.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;
use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// substring: exhaustive parameter combinations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_substring_comprehensive_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic: START only
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 0)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 3)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 7)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 8)"#);

    // START and END both positive
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 0 0)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 0 8)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 2 5)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 4 4)"#);

    // Negative START (counts from end)
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -8)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -3)"#);

    // Negative END
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 0 -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 0 -7)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 2 -2)"#);

    // Both negative
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -6 -2)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -4 -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -8 -0)"#);

    // Positive START, negative END
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 1 -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" 3 -2)"#);

    // Single character string
    assert_oracle_parity_with_bootstrap(r#"(substring "x" 0)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "x" 0 1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "x" -1)"#);

    // Empty result
    assert_oracle_parity_with_bootstrap(r#"(substring "hello" 3 3)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "" 0)"#);

    // Combined with let to test programmatic usage
    let form = r#"(let ((s "The quick brown fox jumps"))
                    (list (substring s 4 9)
                          (substring s -5)
                          (substring s 10 -5)
                          (substring s -15 -10)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// concat: 0, 1, 2, 3+ args, mixed string/char-list/vector types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_concat_comprehensive_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Zero args
    assert_oracle_parity_with_bootstrap(r#"(concat)"#);

    // Single arg of each type
    assert_oracle_parity_with_bootstrap(r#"(concat "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(concat '(104 101 108 108 111))"#);
    assert_oracle_parity_with_bootstrap(r#"(concat [104 101 108 108 111])"#);
    assert_oracle_parity_with_bootstrap(r#"(concat nil)"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "")"#);

    // Two args, all type combinations
    assert_oracle_parity_with_bootstrap(r#"(concat "hel" "lo")"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "hel" '(108 111))"#);
    assert_oracle_parity_with_bootstrap(r#"(concat '(104 101) "llo")"#);
    assert_oracle_parity_with_bootstrap(r#"(concat [104 101] [108 108 111])"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "abc" [100 101 102])"#);

    // Three+ args mixed
    assert_oracle_parity_with_bootstrap(r#"(concat "a" '(98) [99] "d" nil "e")"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "" "" "" "" "")"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "x" nil nil nil "y")"#);

    // Concat building up a string in a loop
    let form = r#"(let ((result "")
                        (words '("the" "quick" "brown" "fox")))
                    (dolist (w words)
                      (setq result (concat result (if (string= result "") "" " ") w)))
                    result)"#;
    assert_oracle_parity_with_bootstrap(form);

    // Many arguments via apply
    let form2 = r#"(apply #'concat (mapcar #'number-to-string '(1 2 3 4 5 6 7 8 9 0)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Concat with multibyte characters
    assert_oracle_parity_with_bootstrap(r#"(concat '(955 945 956 946 948 945))"#);
}

// ---------------------------------------------------------------------------
// string-replace: comprehensive usage
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_replace_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic replacement
    assert_oracle_parity_with_bootstrap(r#"(string-replace "foo" "bar" "foo baz foo")"#);

    // No match
    assert_oracle_parity_with_bootstrap(r#"(string-replace "xyz" "abc" "hello world")"#);

    // Replace with empty string (deletion)
    assert_oracle_parity_with_bootstrap(r#"(string-replace "l" "" "hello")"#);

    // Replace empty with something (inserts between every char)
    assert_oracle_parity_with_bootstrap(r#"(string-replace "" "-" "abc")"#);

    // Overlapping potential matches (non-regex, literal)
    assert_oracle_parity_with_bootstrap(r#"(string-replace "aa" "b" "aaa")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-replace "aa" "b" "aaaa")"#);

    // Replacement longer than original
    assert_oracle_parity_with_bootstrap(r#"(string-replace "a" "xyz" "banana")"#);

    // Multi-character FROMSTRING
    assert_oracle_parity_with_bootstrap(r#"(string-replace "the" "a" "the cat in the hat")"#);

    // Replace in empty string
    assert_oracle_parity_with_bootstrap(r#"(string-replace "a" "b" "")"#);

    // Self-replacement (idempotent)
    assert_oracle_parity_with_bootstrap(r#"(string-replace "foo" "foo" "foo bar foo")"#);

    // Chained replacements
    let form = r#"(let ((s "hello world"))
                    (setq s (string-replace "hello" "goodbye" s))
                    (setq s (string-replace "world" "planet" s))
                    s)"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string: FIXEDCASE, LITERAL, SUBEXP, START
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_comprehensive_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic regex replacement
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "[0-9]+" "NUM" "abc123def456")"#,
    );

    // With backreference in replacement
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "\\([a-z]+\\)" "[\\1]" "hello world foo")"#,
    );

    // FIXEDCASE = t (preserve case of original)
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "hello" "goodbye" "Hello HELLO hello" t)"#,
    );

    // FIXEDCASE = nil (default)
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "hello" "goodbye" "Hello HELLO hello" nil)"#,
    );

    // LITERAL = t (treat replacement as literal, no backslash processing)
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "\\([a-z]+\\)" "\\1" "hello world" nil t)"#,
    );
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "\\([a-z]+\\)" "\\1" "hello world" nil nil)"#,
    );

    // SUBEXP parameter: replace only specific subexpression
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "\\(foo\\)\\(bar\\)" "BAZ" "foobar baz foobar" nil nil nil 1)"#,
    );
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "\\(foo\\)\\(bar\\)" "BAZ" "foobar baz foobar" nil nil nil 2)"#,
    );

    // START parameter: begin matching from offset
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "[0-9]+" "N" "a1b2c3d4" nil nil nil nil 4)"#,
    );

    // Replace with empty
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "[[:space:]]+" "" "  hello   world  ")"#,
    );

    // Replace character classes
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "[[:upper:]]" "x" "Hello World FOO")"#,
    );

    // Dot matches
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "a.b" "X" "aXb a1b a\nb acb")"#,
    );

    // Anchored replacements
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "^hello" "goodbye" "hello world")"#,
    );
    assert_oracle_parity_with_bootstrap(
        r#"(replace-regexp-in-string "world$" "planet" "hello world")"#,
    );
}

// ---------------------------------------------------------------------------
// split-string: SEPARATORS, OMIT-NULLS, TRIM params -- comprehensive
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_split_string_comprehensive_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Default separator (whitespace), default OMIT-NULLS (t)
    assert_oracle_parity_with_bootstrap(r#"(split-string "  hello   world  ")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "\t\nhello\t\nworld\t\n")"#);

    // Custom separator, OMIT-NULLS default
    assert_oracle_parity_with_bootstrap(r#"(split-string "a:b:c:d" ":")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "a::b:::c" ":")"#);

    // OMIT-NULLS = nil (keep empty strings)
    assert_oracle_parity_with_bootstrap(r#"(split-string "a,,b,,c" "," nil)"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string ",a,b,c," "," nil)"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string ",,," "," nil)"#);

    // OMIT-NULLS = t
    assert_oracle_parity_with_bootstrap(r#"(split-string "a,,b,,c" "," t)"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string ",,," "," t)"#);

    // TRIM parameter (regex to trim from each resulting piece)
    assert_oracle_parity_with_bootstrap(r#"(split-string " a , b , c " "," t " ")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "  x  |  y  |  z  " "|" t "[ \t]+")"#);

    // Multi-char regex separator
    assert_oracle_parity_with_bootstrap(r#"(split-string "one-->two-->three" "-->")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "a123b456c" "[0-9]+")"#);

    // Edge cases
    assert_oracle_parity_with_bootstrap(r#"(split-string "" ",")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "nosep" ",")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "," ",")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "," "," nil)"#);

    // Complex: split CSV-like, trim whitespace
    let form = r#"(split-string "  alpha = 1 , beta = 2 , gamma = 3  " "," t "[ \t]+")"#;
    assert_oracle_parity_with_bootstrap(form);

    // Roundtrip: split then join back
    let form2 = r#"(let ((parts (split-string "a/b/c/d" "/")))
                      (string-join parts "/"))"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// string-join: various separators and edge cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_join_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Normal separators
    assert_oracle_parity_with_bootstrap(r#"(string-join '("a" "b" "c") ", ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("x" "y" "z") " | ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("1" "2" "3") "")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("one" "two" "three") " and ")"#);

    // Single element list
    assert_oracle_parity_with_bootstrap(r#"(string-join '("only") ":::")"#);

    // Empty list
    assert_oracle_parity_with_bootstrap(r#"(string-join nil ",")"#);

    // Default separator (no second arg)
    assert_oracle_parity_with_bootstrap(r#"(string-join '("a" "b" "c"))"#);

    // Join with newline
    assert_oracle_parity_with_bootstrap(r#"(string-join '("line1" "line2" "line3") "\n")"#);

    // Join empty strings
    assert_oracle_parity_with_bootstrap(r#"(string-join '("" "" "") ",")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("" "mid" "") ",")"#);

    // Join from computed list
    let form = r#"(string-join
                    (mapcar (lambda (n) (format "item-%d" n))
                            '(1 2 3 4 5))
                    " -> ")"#;
    assert_oracle_parity_with_bootstrap(form);

    // Nested join/split roundtrip
    let form2 = r#"(let ((csv "name,age,city"))
                      (equal csv (string-join (split-string csv ",") ",")))"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// string-trim, string-trim-left, string-trim-right with custom TRIM-CHARS
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_trim_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Default whitespace trimming
    assert_oracle_parity_with_bootstrap(r#"(string-trim "   hello   ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "\t\n hello \n\t")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "   ")"#);

    // Custom TRIM-CHARS (character alternatives for regex)
    assert_oracle_parity_with_bootstrap(r#"(string-trim "---hello---" "-")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "***wrap***" "*")"#);
    assert_oracle_parity_with_bootstrap(r###"(string-trim "##title##" "#")"###);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "+-=val=-+" "[+=\\-]")"#);

    // Left trim only
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left ">>>hello<<<" ">")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "000123" "0")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "   hello   ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "hello" "x")"#);

    // Right trim only
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "hello..." ".")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "hello<<<" "<")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "   hello   ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "hello" "x")"#);

    // Trim entire string
    assert_oracle_parity_with_bootstrap(r#"(string-trim "---" "-")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "---" "-")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "---" "-")"#);

    // Trim with regex character classes
    assert_oracle_parity_with_bootstrap(r#"(string-trim "123hello456" "[0-9]")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "ABChelloXYZ" "[A-Z]")"#);

    // Combine trim operations
    let form = r#"(let ((s "  ### TITLE ###  "))
                    (list (string-trim s)
                          (string-trim (string-trim s) "[# ]")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-chop-newline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_chop_newline_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "hello\n")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "\n")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "hello\n\n")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "\nhello\n")"#);

    // Only removes trailing newline, not carriage return
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "hello\r\n")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-chop-newline "hello\r")"#);
}

// ---------------------------------------------------------------------------
// Complex: string pipeline combining multiple operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_manipulation_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a slug generator: trim, downcase, replace spaces/special with hyphens
    let form = r#"(let ((title "  Hello, World! This is a TEST.  "))
                    (let* ((trimmed (string-trim title))
                           (lowered (downcase trimmed))
                           (no-special (replace-regexp-in-string "[^a-z0-9 ]" "" lowered))
                           (hyphenated (replace-regexp-in-string " +" "-" no-special)))
                      (list trimmed lowered no-special hyphenated)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // CSV parser: split lines, split fields, trim each
    let form2 = r#"(let ((csv "name, age, city\nAlice, 30, NYC\nBob, 25, LA"))
                      (let ((lines (split-string csv "\n" t)))
                        (mapcar
                          (lambda (line)
                            (mapcar
                              (lambda (field) (string-trim field))
                              (split-string line "," nil)))
                          lines)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Word frequency counter using string operations
    let form3 = r#"(let* ((text "the cat sat on the mat the cat")
                          (words (split-string (downcase text) " " t))
                          (counts nil))
                     (dolist (w words)
                       (let ((entry (assoc w counts)))
                         (if entry
                             (setcdr entry (1+ (cdr entry)))
                           (setq counts (cons (cons w 1) counts)))))
                     (sort counts (lambda (a b) (> (cdr a) (cdr b)))))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// Complex: string-replace vs replace-regexp-in-string interaction
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_replace_vs_regexp_replace() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-replace is literal (not regex)
    assert_oracle_parity_with_bootstrap(r#"(string-replace "." "!" "a.b.c")"#);
    // replace-regexp-in-string treats . as any char
    assert_oracle_parity_with_bootstrap(r#"(replace-regexp-in-string "\\." "!" "a.b.c")"#);

    // Bracket differences
    assert_oracle_parity_with_bootstrap(r#"(string-replace "[x]" "Y" "a[x]b[x]c")"#);
    assert_oracle_parity_with_bootstrap(r#"(replace-regexp-in-string "\\[x\\]" "Y" "a[x]b[x]c")"#);

    // Backslash handling
    assert_oracle_parity_with_bootstrap(r#"(string-replace "+" "plus" "1+2+3")"#);
    assert_oracle_parity_with_bootstrap(r#"(replace-regexp-in-string "\\+" "plus" "1+2+3")"#);

    // Multi-step transformation
    let form = r#"(let ((s "Hello World 123 Foo"))
                    (list
                      ;; Literal replacements
                      (string-replace "Hello" "Hi" s)
                      ;; Regex: remove all digits
                      (replace-regexp-in-string "[0-9]" "" s)
                      ;; Regex: wrap words in brackets
                      (replace-regexp-in-string "\\([A-Za-z]+\\)" "[\\1]" s)
                      ;; Regex with START offset
                      (replace-regexp-in-string "[A-Z]" "x" s nil nil nil nil 6)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
