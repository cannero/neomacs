//! Oracle parity tests for complex string processing patterns:
//! `split-string`, `string-join`, `string-trim`, `string-prefix-p`,
//! `string-suffix-p`, `replace-regexp-in-string` in combination.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// split-string extended usage
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_split_string_default_sep() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Default separator is split-string-default-separators
    assert_oracle_parity_with_bootstrap(r#"(split-string "  hello   world  ")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "no-spaces")"#);
}

#[test]
fn oracle_prop_split_string_custom_separator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(split-string "a,b,c,d" ",")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "a::b::c" "::")"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string "a.b.c" "\\.")"#);
}

#[test]
fn oracle_prop_split_string_omit_nulls() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // OMIT-NULLS parameter (3rd arg)
    assert_oracle_parity_with_bootstrap(r#"(split-string ",a,,b,c," "," t)"#);
    assert_oracle_parity_with_bootstrap(r#"(split-string ",a,,b,c," ",")"#);
}

#[test]
fn oracle_prop_split_string_trim() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // TRIM parameter (4th arg) — regex to trim from each result
    assert_oracle_parity_with_bootstrap(r#"(split-string "  a , b , c  " "," t "[ \t]+")"#);
}

// ---------------------------------------------------------------------------
// string-trim / string-trim-left / string-trim-right
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_trim_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-trim "  hello  ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-left "  hello  ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim-right "  hello  ")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "\n\thello\n\t")"#);
}

#[test]
fn oracle_prop_string_trim_custom_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Custom trim characters
    assert_oracle_parity_with_bootstrap(r#"(string-trim "---hello---" "-+")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-trim "***hello***" "[*]+" "[*]+")"#);
}

// ---------------------------------------------------------------------------
// string-prefix-p / string-suffix-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_prefix_suffix() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "hel" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "world" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "hello" "hello")"#);

    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "llo" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "world" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "" "hello")"#);
}

#[test]
fn oracle_prop_string_prefix_ignore_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // IGNORE-CASE parameter (3rd arg)
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "HEL" "hello" t)"#);
    assert_oracle_parity_with_bootstrap(r#"(string-prefix-p "HEL" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-suffix-p "LLO" "hello" t)"#);
}

// ---------------------------------------------------------------------------
// string-search / string-replace
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_search_positions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-search "world" "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-search "xyz" "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-search "o" "hello world")"#);
}

#[test]
fn oracle_prop_string_search_start_pos() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // START-POS parameter
    assert_oracle_parity_with_bootstrap(r#"(string-search "o" "hello world" 5)"#);
    assert_oracle_parity_with_bootstrap(r#"(string-search "o" "hello world" 8)"#);
}

#[test]
fn oracle_prop_string_replace_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-replace "world" "emacs" "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-replace "o" "0" "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-replace "xyz" "abc" "hello world")"#);
}

// ---------------------------------------------------------------------------
// Complex: text processing pipelines
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_csv_parser() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Parse CSV-like data into structured form
    let form = r####"(let ((csv "name,age,role\nAlice,30,engineer\nBob,25,designer"))
                    (let ((lines (split-string csv "\n"))
                          (result nil))
                      (let ((headers (split-string (car lines) ",")))
                        (dolist (line (cdr lines))
                          (let ((values (split-string line ","))
                                (record nil)
                                (h headers))
                            (while (and h values)
                              (setq record
                                    (cons (cons (car h) (car values))
                                          record))
                              (setq h (cdr h) values (cdr values)))
                            (setq result (cons (nreverse record) result)))))
                      (nreverse result)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_string_path_manipulation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Path manipulation: split, normalize, rebuild
    let form = r####"(let ((path "/home/user/../user/./docs/file.txt"))
                    ;; Split into components
                    (let ((parts (split-string path "/" t)))
                      ;; Normalize: remove "." and handle ".."
                      (let ((stack nil))
                        (dolist (p parts)
                          (cond
                            ((string= p ".") nil)
                            ((string= p "..")
                             (when stack (setq stack (cdr stack))))
                            (t (setq stack (cons p stack)))))
                        ;; Rebuild
                        (concat "/" (mapconcat #'identity
                                              (nreverse stack) "/")))))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_string_word_wrap() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Word-wrap text at given width
    let form = r####"(let ((text "the quick brown fox jumps over the lazy dog")
                        (width 15))
                    (let ((words (split-string text))
                          (lines nil)
                          (current-line ""))
                      (dolist (word words)
                        (if (= (length current-line) 0)
                            (setq current-line word)
                          (if (<= (+ (length current-line) 1 (length word))
                                  width)
                              (setq current-line
                                    (concat current-line " " word))
                            (setq lines (cons current-line lines)
                                  current-line word))))
                      (when (> (length current-line) 0)
                        (setq lines (cons current-line lines)))
                      (nreverse lines)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_string_camelcase_to_kebab() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Convert camelCase to kebab-case
    let form = r####"(let ((convert
                     (lambda (s)
                       (let ((result "")
                             (i 0)
                             (len (length s)))
                         (while (< i len)
                           (let ((ch (aref s i)))
                             (if (and (>= ch ?A) (<= ch ?Z))
                                 (setq result
                                       (concat result
                                               (if (> i 0) "-" "")
                                               (char-to-string
                                                (+ ch 32))))
                               (setq result
                                     (concat result
                                             (char-to-string ch)))))
                           (setq i (1+ i)))
                         result))))
                    (list (funcall convert "helloWorld")
                          (funcall convert "camelCaseString")
                          (funcall convert "XMLParser")
                          (funcall convert "simple")))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_string_frequency_analysis() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Character frequency analysis
    let form = r####"(let ((text "hello world")
                        (freq (make-hash-table)))
                    (dotimes (i (length text))
                      (let ((ch (aref text i)))
                        (puthash ch (1+ (gethash ch freq 0)) freq)))
                    ;; Get sorted frequency list
                    (let ((pairs nil))
                      (maphash (lambda (k v)
                                 (setq pairs
                                       (cons (cons (char-to-string k) v)
                                             pairs)))
                               freq)
                      (sort pairs
                            (lambda (a b)
                              (> (cdr a) (cdr b))))))"####;
    assert_oracle_parity_with_bootstrap(form);
}
