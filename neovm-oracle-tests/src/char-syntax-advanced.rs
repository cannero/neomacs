//! Advanced oracle parity tests for `char-syntax`, `syntax-class-to-char`,
//! `matching-paren`, `string-to-syntax`, and `modify-syntax-entry` interactions.
//!
//! Covers: char-syntax for various character categories, syntax after modification,
//! syntax-class-to-char mapping, matching-paren for delimiters, string-to-syntax
//! descriptor creation, and a syntax-aware tokenizer built on char-syntax.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// char-syntax for letters, digits, whitespace, punctuation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_basic_categories() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Query char-syntax for representative characters in the standard syntax table
    // and verify their syntax class characters
    let form = r#"(with-temp-buffer
                    ;; Use the standard syntax table
                    (list
                     ;; Letters are word constituents -> ?w
                     (char-syntax ?a) (char-syntax ?z)
                     (char-syntax ?A) (char-syntax ?Z)
                     ;; Digits are word constituents -> ?w
                     (char-syntax ?0) (char-syntax ?9)
                     ;; Space and tab are whitespace -> ?\s (32)
                     (char-syntax ?\s) (char-syntax ?\t)
                     ;; Newline
                     (char-syntax ?\n)
                     ;; Common punctuation
                     (char-syntax ?.) (char-syntax ?,)
                     (char-syntax ?!) (char-syntax ??)
                     (char-syntax ?+) (char-syntax ?-)
                     (char-syntax ?*) (char-syntax ?/)
                     ;; Brackets
                     (char-syntax ?\() (char-syntax ?\))
                     (char-syntax ?\[) (char-syntax ?\])
                     ;; String delimiter
                     (char-syntax ?\")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// char-syntax after modify-syntax-entry changes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_after_modification() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Modify syntax entries and verify char-syntax reflects the changes,
    // then undo by setting back and verify restoration
    let form = r#"(with-temp-buffer
                    (let ((st (make-syntax-table)))
                      (set-syntax-table st)
                      ;; Record originals
                      (let ((orig-at (char-syntax ?@))
                            (orig-hash (char-syntax ?#))
                            (orig-dollar (char-syntax ?$))
                            (orig-pipe (char-syntax ?|)))
                        ;; Modify: @ -> word, # -> comment-start, $ -> symbol, | -> string fence
                        (modify-syntax-entry ?@ "w" st)
                        (modify-syntax-entry ?# "<" st)
                        (modify-syntax-entry ?$ "_" st)
                        (modify-syntax-entry ?| "|" st)
                        (let ((mod-at (char-syntax ?@))
                              (mod-hash (char-syntax ?#))
                              (mod-dollar (char-syntax ?$))
                              (mod-pipe (char-syntax ?|)))
                          ;; Restore @ back to punctuation
                          (modify-syntax-entry ?@ "." st)
                          (let ((restored-at (char-syntax ?@)))
                            (list orig-at mod-at restored-at
                                  orig-hash mod-hash
                                  orig-dollar mod-dollar
                                  orig-pipe mod-pipe
                                  ;; Verify changes happened
                                  (not (= orig-at mod-at))
                                  (= restored-at (char-to-string ?.))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// syntax-class-to-char mapping for all 16 classes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_class_to_char_all() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Map all 16 syntax class codes to their character representations
    // and verify they form the expected set
    let form = r#"(let ((results nil))
                    ;; Classes 0-15: whitespace, word, symbol, punctuation,
                    ;; open, close, string-quote, escape, paired-delim,
                    ;; char-quote, comment-start, comment-end,
                    ;; inherit, generic-comment, generic-string, expression-prefix
                    (dotimes (i 16)
                      (let ((ch (syntax-class-to-char i)))
                        (setq results (cons (cons i ch) results))))
                    ;; Verify specific known mappings
                    (let ((mapping (nreverse results)))
                      (list mapping
                            ;; Spot checks
                            (= (cdr (assq 0 mapping)) ?\s)   ;; whitespace -> space
                            (= (cdr (assq 1 mapping)) ?w)    ;; word -> w
                            (= (cdr (assq 2 mapping)) ?_)    ;; symbol -> _
                            (= (cdr (assq 3 mapping)) ?.)    ;; punctuation -> .
                            (= (cdr (assq 4 mapping)) ?\()   ;; open -> (
                            (= (cdr (assq 5 mapping)) ?\))   ;; close -> )
                            (= (cdr (assq 6 mapping)) ?\")   ;; string -> "
                            (= (cdr (assq 7 mapping)) ?\\))) ;; escape -> \
                    )"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// matching-paren for standard and custom paired delimiters
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_matching_paren_extended() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test matching-paren for standard parens, brackets, braces,
    // custom angle brackets, and non-bracket characters
    let form = r#"(with-temp-buffer
                    (let ((st (make-syntax-table)))
                      ;; Standard parens/brackets/braces already defined in standard table
                      ;; Add angle brackets as custom paired delimiters
                      (modify-syntax-entry ?< "(>" st)
                      (modify-syntax-entry ?> ")<" st)
                      ;; Add guillemets as brackets
                      (modify-syntax-entry 171 "(187" st)  ;; left guillemet
                      (modify-syntax-entry 187 ")171" st)  ;; right guillemet
                      (set-syntax-table st)
                      (list
                       ;; Standard pairs
                       (matching-paren ?\()     ;; -> ?\)
                       (matching-paren ?\))     ;; -> ?\(
                       (matching-paren ?\[)     ;; -> ?\]
                       (matching-paren ?\])     ;; -> ?\[
                       (matching-paren ?{)      ;; -> ?}
                       (matching-paren ?})      ;; -> ?{
                       ;; Custom angle brackets
                       (matching-paren ?<)      ;; -> ?>
                       (matching-paren ?>)      ;; -> ?<
                       ;; Non-bracket returns nil
                       (matching-paren ?a)
                       (matching-paren ?+)
                       (matching-paren ?\s)
                       ;; Guillemets
                       (matching-paren 171)
                       (matching-paren 187))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-to-syntax creating syntax descriptors
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_string_to_syntax() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-to-syntax parses a syntax descriptor string into a cons (code . match-char)
    // Test various descriptor formats
    let form = r#"(list
                    ;; Simple class descriptors
                    (string-to-syntax " ")    ;; whitespace
                    (string-to-syntax "w")    ;; word
                    (string-to-syntax "_")    ;; symbol
                    (string-to-syntax ".")    ;; punctuation
                    (string-to-syntax "\"")   ;; string quote
                    (string-to-syntax "\\")   ;; escape
                    (string-to-syntax "'")    ;; expression prefix
                    (string-to-syntax "<")    ;; comment start
                    (string-to-syntax ">")    ;; comment end
                    (string-to-syntax "!")    ;; generic comment
                    (string-to-syntax "|")    ;; generic string
                    ;; Paired delimiters with matching char
                    (string-to-syntax "()")   ;; open paren matching )
                    (string-to-syntax ")(")   ;; close paren matching (
                    (string-to-syntax "(>")   ;; open matching >
                    (string-to-syntax ")<")   ;; close matching <
                    ;; With comment flags
                    (string-to-syntax ". 1")  ;; punct with comment-start first char flag
                    (string-to-syntax ". 2")  ;; punct with comment-start second char flag
                    (string-to-syntax ". 3")  ;; punct with comment-end first char flag
                    (string-to-syntax ". 4")  ;; punct with comment-end second char flag
                    ;; Verify they are cons cells
                    (consp (string-to-syntax "w"))
                    (integerp (car (string-to-syntax "w"))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build a syntax-aware tokenizer using char-syntax
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_tokenizer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a tokenizer that classifies each character by its syntax class
    // using char-syntax, then groups consecutive same-class chars into tokens
    let form = r#"(progn
                    (fset 'neovm--test-char-syntax-tokenize
                          (lambda (text)
                            "Tokenize TEXT by grouping consecutive chars of same syntax class."
                            (with-temp-buffer
                              (let ((st (make-syntax-table)))
                                (modify-syntax-entry ?_ "_" st)
                                (modify-syntax-entry '(?a . ?z) "w" st)
                                (modify-syntax-entry '(?A . ?Z) "w" st)
                                (modify-syntax-entry '(?0 . ?9) "w" st)
                                (modify-syntax-entry ?+ "." st)
                                (modify-syntax-entry ?- "." st)
                                (modify-syntax-entry ?* "." st)
                                (modify-syntax-entry ?= "." st)
                                (modify-syntax-entry ?\( "()" st)
                                (modify-syntax-entry ?\) ")(" st)
                                (set-syntax-table st)
                                (insert text)
                                (goto-char (point-min))
                                (let ((tokens nil))
                                  (while (< (point) (point-max))
                                    (let* ((ch (char-after (point)))
                                           (syn (char-syntax ch))
                                           (class-name
                                            (cond
                                             ((= syn ?\s) 'ws)
                                             ((= syn ?w) 'word)
                                             ((= syn ?_) 'sym)
                                             ((= syn ?.) 'punct)
                                             ((= syn ?\() 'open)
                                             ((= syn ?\)) 'close)
                                             ((= syn ?\") 'str)
                                             (t 'other)))
                                           (start (point)))
                                      ;; Accumulate chars of the same syntax class
                                      (forward-char 1)
                                      (while (and (< (point) (point-max))
                                                  (= (char-syntax (char-after (point))) syn))
                                        (forward-char 1))
                                      (setq tokens
                                            (cons (list class-name
                                                        (buffer-substring start (point)))
                                                  tokens))))
                                  (nreverse tokens))))))
                    (unwind-protect
                        (list
                         (neovm--test-char-syntax-tokenize "x = foo(42) + bar_baz")
                         (neovm--test-char-syntax-tokenize "  a + b  ")
                         (neovm--test-char-syntax-tokenize "hello"))
                      (fmakunbound 'neovm--test-char-syntax-tokenize)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: char-syntax with buffer-local syntax tables and switching
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_adv_buffer_local_switching() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Create two buffers with different syntax tables, switch between them,
    // and verify char-syntax respects the current buffer's table
    let form = r#"(let ((st-lisp (make-syntax-table))
                        (st-c (make-syntax-table)))
                    ;; Lisp-like: - is symbol, ; is comment-start
                    (modify-syntax-entry ?- "_" st-lisp)
                    (modify-syntax-entry ?\; "<" st-lisp)
                    (modify-syntax-entry ?_ "_" st-lisp)
                    ;; C-like: - is punctuation, ; is punctuation, _ is word
                    (modify-syntax-entry ?- "." st-c)
                    (modify-syntax-entry ?\; "." st-c)
                    (modify-syntax-entry ?_ "w" st-c)
                    (let ((results nil))
                      (with-temp-buffer
                        (rename-buffer " *test-lisp*" t)
                        (set-syntax-table st-lisp)
                        ;; In lisp buffer
                        (let ((lisp-dash (char-syntax ?-))
                              (lisp-semi (char-syntax ?\;))
                              (lisp-under (char-syntax ?_)))
                          (with-temp-buffer
                            (rename-buffer " *test-c*" t)
                            (set-syntax-table st-c)
                            ;; In C buffer
                            (let ((c-dash (char-syntax ?-))
                                  (c-semi (char-syntax ?\;))
                                  (c-under (char-syntax ?_)))
                              (setq results
                                    (list lisp-dash lisp-semi lisp-under
                                          c-dash c-semi c-under
                                          ;; Verify they differ
                                          (not (= lisp-dash c-dash))
                                          (not (= lisp-semi c-semi))
                                          (not (= lisp-under c-under))))))))
                      results))"#;
    assert_oracle_parity_with_bootstrap(form);
}
