//! Oracle parity tests for advanced string manipulation:
//! substring with negative indices, concat with chars, split-string params,
//! string-join, string-trim with custom chars, string-prefix-p / string-suffix-p,
//! upcase-initials, and complex path manipulation.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// substring with negative indices (from end)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_substring_negative_indices() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Negative FROM counts from end
    assert_oracle_parity_with_bootstrap(r#"(substring "hello world" -5)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "hello world" -5 -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdef" -3)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdef" -4 -1)"#);

    // Negative TO only
    assert_oracle_parity_with_bootstrap(r#"(substring "hello world" 0 -1)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "hello world" 0 -6)"#);

    // Both negative
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -6 -2)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdefgh" -1)"#);

    // Edge: last char
    assert_oracle_parity_with_bootstrap(r#"(substring "x" -1)"#);

    // Combining positive start with negative end
    assert_oracle_parity_with_bootstrap(r#"(substring "hello world" 3 -3)"#);
    assert_oracle_parity_with_bootstrap(r#"(substring "abcdef" 1 -1)"#);

    // Full string via negative
    assert_oracle_parity_with_bootstrap(r#"(substring "test" -4)"#);
}

// ---------------------------------------------------------------------------
// concat with many args including chars
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_concat_many_args_with_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic multi-arg concat
    assert_oracle_parity_with_bootstrap(r#"(concat "a" "b" "c" "d" "e")"#);

    // Mix strings and empty strings
    assert_oracle_parity_with_bootstrap(r#"(concat "" "hello" "" " " "" "world" "")"#);

    // Concat with nil (nil is ignored in concat)
    assert_oracle_parity_with_bootstrap(r#"(concat "a" nil "b" nil "c")"#);

    // Concat with lists of chars
    assert_oracle_parity_with_bootstrap(r#"(concat '(72 101 108 108 111))"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "He" '(108 108) "o")"#);

    // Concat with vectors of chars
    assert_oracle_parity_with_bootstrap(r#"(concat [72 101 108 108 111])"#);
    assert_oracle_parity_with_bootstrap(r#"(concat "He" [108 108] "o")"#);

    // Many small strings
    let form = r####"(let ((parts nil))
                    (dotimes (i 10)
                      (setq parts (cons (number-to-string i) parts)))
                    (apply #'concat (nreverse parts)))"####;
    assert_oracle_parity_with_bootstrap(form);

    // Zero args
    assert_oracle_parity_with_bootstrap(r#"(concat)"#);
}

// ---------------------------------------------------------------------------
// split-string with all params (STRING, SEPARATORS, OMIT-NULLS, TRIM)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_split_string_full_params() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Default separator (whitespace)
    assert_oracle_parity_with_bootstrap(r#"(split-string "  hello   world  ")"#);

    // Custom separator
    assert_oracle_parity_with_bootstrap(r#"(split-string "a,b,,c,d" ",")"#);

    // OMIT-NULLS = nil (keep empty strings)
    assert_oracle_parity_with_bootstrap(r#"(split-string "a,,b,,c" "," nil)"#);

    // OMIT-NULLS = t (remove empty strings)
    assert_oracle_parity_with_bootstrap(r#"(split-string "a,,b,,c" "," t)"#);

    // TRIM parameter
    assert_oracle_parity_with_bootstrap(r#"(split-string " a , b , c " "," t " ")"#);

    // Multi-character separator regex
    assert_oracle_parity_with_bootstrap(r#"(split-string "one--two--three" "--")"#);

    // Splitting on newlines
    assert_oracle_parity_with_bootstrap(r#"(split-string "line1\nline2\nline3" "\n")"#);

    // No matches for separator
    assert_oracle_parity_with_bootstrap(r#"(split-string "hello" ",")"#);

    // Empty string
    assert_oracle_parity_with_bootstrap(r#"(split-string "" ",")"#);

    // Separator at edges
    assert_oracle_parity_with_bootstrap(r#"(split-string ",a,b,c," "," nil)"#);
}

// ---------------------------------------------------------------------------
// string-join with separator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_join_advanced() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Various separators
    assert_oracle_parity_with_bootstrap(r#"(string-join '("a" "b" "c") ", ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("x" "y" "z") " -> ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("1" "2" "3") "")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join '("one") "--")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-join nil ",")"#);

    // Join with newline separator
    assert_oracle_parity_with_bootstrap(r#"(string-join '("line1" "line2" "line3") "\n")"#);

    // Roundtrip: split then join
    let form = r####"(string-join (split-string "a-b-c" "-") "+")"####;
    assert_oracle_parity_with_bootstrap(form);

    // Join many elements
    let form2 = r#"(let ((items nil))
                     (dotimes (i 8)
                       (setq items (cons (format "item%d" i) items)))
                     (string-join (nreverse items) "|"))"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// string-trim / string-trim-left / string-trim-right with custom chars
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_trim_custom_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Default trim (whitespace)
    assert_oracle_parity_with_bootstrap(r#"(string-trim " \t\n hello \t\n ")"#);

    // Custom trim chars
    assert_oracle_parity_with_bootstrap(r#"(string-trim "---hello---" "-")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "***hello***" "*")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "//path//" "/")"#);

    // Trim-left only
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left ">>>hello<<<" ">")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "  hello  ")"#);

    // Trim-right only
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right ">>>hello<<<" "<")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "hello..."  ".")"#);

    // Multiple custom chars in trim set
    assert_oracle_parity_with_bootstrap(r#"(string-trim "+-=hello=-+" "+-=")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "+-=hello=-+" "+-=")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "+-=hello=-+" "+-=")"#);

    // Nothing to trim
    assert_oracle_parity_with_bootstrap(r#"(string-trim "hello" "x")"#);

    // Entire string is trim chars
    assert_oracle_parity_with_bootstrap(r#"(string-trim "---" "-")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "" "-")"#);
}

// ---------------------------------------------------------------------------
// string-prefix-p / string-suffix-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_prefix_suffix_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Prefix checks
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "hel" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "hello" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "world" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "hello!" "hello")"#);

    // Suffix checks
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "llo" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "hello" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "world" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "hello!" "hello")"#);

    // Case-insensitive prefix
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "HEL" "hello" t)"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "HEL" "hello" nil)"#);

    // Case-insensitive suffix
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "LLO" "hello" t)"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "LLO" "hello" nil)"#);

    // With empty string
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "" "")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "" "")"#);
}

// ---------------------------------------------------------------------------
// upcase-initials for title case
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_upcase_initials() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "HELLO WORLD")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "hELLO wORLD")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "a")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "hello-world")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "one two three four")"#);

    // With non-alpha separators
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "foo_bar_baz")"#);
    assert_oracle_parity_with_bootstrap(r#"(upcase-initials "foo.bar.baz")"#);
}

// ---------------------------------------------------------------------------
// Complex: string-based path manipulation (join, split, normalize)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_path_manipulation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a path manipulation toolkit using string primitives
    let form = r####"(let ((path-split
                         (lambda (path)
                           (split-string path "/" t)))
                        (path-join
                         (lambda (parts)
                           (concat "/" (string-join parts "/"))))
                        (path-dirname
                         (lambda (path)
                           (let ((parts (split-string path "/" t)))
                             (if (> (length parts) 1)
                                 (concat "/"
                                         (string-join (butlast parts) "/"))
                               "/"))))
                        (path-basename
                         (lambda (path)
                           (let ((parts (split-string path "/" t)))
                             (if parts (car (last parts)) ""))))
                        (path-extension
                         (lambda (path)
                           (let ((base (car (last (split-string path "/" t)))))
                             (if (and base (string-match "\\." base))
                                 (let ((parts (split-string base "\\." t)))
                                   (car (last parts)))
                               nil))))
                        (path-normalize
                         (lambda (path)
                           (let ((parts (split-string path "/" t))
                                 (stack nil))
                             (dolist (part parts)
                               (cond
                                ((string= part "."))
                                ((string= part "..")
                                 (when stack (setq stack (cdr stack))))
                                (t (setq stack (cons part stack)))))
                             (concat "/" (string-join (nreverse stack) "/"))))))
                    (list
                     ;; Split
                     (funcall path-split "/usr/local/bin/emacs")
                     ;; Join
                     (funcall path-join '("usr" "local" "bin"))
                     ;; Dirname
                     (funcall path-dirname "/usr/local/bin/emacs")
                     (funcall path-dirname "/single")
                     ;; Basename
                     (funcall path-basename "/usr/local/bin/emacs")
                     (funcall path-basename "/")
                     ;; Extension
                     (funcall path-extension "/home/user/file.txt")
                     (funcall path-extension "/home/user/file")
                     ;; Normalize with . and ..
                     (funcall path-normalize "/usr/local/../share/./emacs")
                     (funcall path-normalize "/a/b/c/../../d")
                     (funcall path-normalize "/a/./b/./c")))"####;
    assert_oracle_parity_with_bootstrap(form);
}
