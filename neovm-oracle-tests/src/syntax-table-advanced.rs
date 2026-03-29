//! Advanced oracle parity tests for syntax tables:
//! parent tables, all syntax classes, copy independence,
//! string-to-syntax / syntax-class-to-char roundtrips, matching-paren
//! with custom brackets, syntax-after in buffers, and DSL-like syntax
//! table construction.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// make-syntax-table with parent: inheritance and override
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_parent_inheritance() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Child inherits from parent but can override entries.
    // Verify parent entries visible through child, and that overrides
    // do not affect the parent.
    let form = r#"(let* ((parent (make-syntax-table))
                         (_ (modify-syntax-entry ?@ "w" parent))
                         (_ (modify-syntax-entry ?# "." parent))
                         (child (make-syntax-table parent)))
                    ;; child inherits @=word from parent
                    (let ((before-child-at (char-syntax ?@))
                          (_ (modify-syntax-entry ?@ "." child))
                          (_ (modify-syntax-entry ?$ "w" child)))
                      (with-temp-buffer
                        ;; Use parent
                        (set-syntax-table parent)
                        (let ((p-at (char-syntax ?@))
                              (p-hash (char-syntax ?#))
                              (p-dollar (char-syntax ?$)))
                          ;; Switch to child
                          (set-syntax-table child)
                          (let ((c-at (char-syntax ?@))
                                (c-hash (char-syntax ?#))
                                (c-dollar (char-syntax ?$)))
                            (list p-at p-hash p-dollar
                                  c-at c-hash c-dollar))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// modify-syntax-entry for all major syntax classes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_all_classes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Set various chars to each syntax class and read them back.
    // Syntax class chars: space=whitespace, w=word, _=symbol, .=punctuation,
    // (=open, )=close, "=string, \=escape, /=char-quote, $=paired-delimiter,
    // '=expression-prefix, <=comment-start, >=comment-end, !=generic-comment,
    // |=generic-string
    let form = r#"(let ((st (make-syntax-table)))
                    (modify-syntax-entry ?A " " st)
                    (modify-syntax-entry ?B "w" st)
                    (modify-syntax-entry ?C "_" st)
                    (modify-syntax-entry ?D "." st)
                    (modify-syntax-entry ?E "(F" st)
                    (modify-syntax-entry ?F ")E" st)
                    (modify-syntax-entry ?G "\"" st)
                    (modify-syntax-entry ?H "\\" st)
                    (modify-syntax-entry ?I "/" st)
                    (modify-syntax-entry ?J "$" st)
                    (modify-syntax-entry ?K "'" st)
                    (modify-syntax-entry ?L "<" st)
                    (modify-syntax-entry ?M ">" st)
                    (modify-syntax-entry ?N "!" st)
                    (modify-syntax-entry ?O "|" st)
                    (with-temp-buffer
                      (set-syntax-table st)
                      (list
                       (char-syntax ?A)   ;; space => 32
                       (char-syntax ?B)   ;; w => 119
                       (char-syntax ?C)   ;; _ => 95
                       (char-syntax ?D)   ;; . => 46
                       (char-syntax ?E)   ;; ( => 40
                       (char-syntax ?F)   ;; ) => 41
                       (char-syntax ?G)   ;; " => 34
                       (char-syntax ?H)   ;; \ => 92
                       (char-syntax ?I)   ;; / => 47
                       (char-syntax ?J)   ;; $ => 36
                       (char-syntax ?K)   ;; ' => 39
                       (char-syntax ?L)   ;; < => 60
                       (char-syntax ?M)   ;; > => 62
                       (char-syntax ?N)   ;; ! => 33
                       (char-syntax ?O))) ;; | => 124
                    )"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// char-syntax with modified syntax tables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_syntax_modified_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify char-syntax reads from current buffer's syntax table
    let form = r#"(with-temp-buffer
                    (let ((st1 (make-syntax-table))
                          (st2 (make-syntax-table)))
                      (modify-syntax-entry ?* "w" st1)
                      (modify-syntax-entry ?* "." st2)
                      ;; Under st1, * is word
                      (set-syntax-table st1)
                      (let ((r1 (char-syntax ?*)))
                        ;; Under st2, * is punctuation
                        (set-syntax-table st2)
                        (let ((r2 (char-syntax ?*)))
                          (list r1 r2 (not (= r1 r2)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// copy-syntax-table independence
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_copy_syntax_table_independence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Modifying the copy must not affect the original.
    let form = r#"(let* ((orig (make-syntax-table))
                         (_ (modify-syntax-entry ?@ "w" orig))
                         (copy (copy-syntax-table orig)))
                    ;; Modify copy
                    (modify-syntax-entry ?@ "." copy)
                    (modify-syntax-entry ?# "_" copy)
                    ;; Check orig is unchanged
                    (with-temp-buffer
                      (set-syntax-table orig)
                      (let ((orig-at (char-syntax ?@))
                            (orig-hash (char-syntax ?#)))
                        (set-syntax-table copy)
                        (let ((copy-at (char-syntax ?@))
                              (copy-hash (char-syntax ?#)))
                          (list orig-at orig-hash
                                copy-at copy-hash
                                (not (= orig-at copy-at)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-to-syntax / syntax-class-to-char roundtrip
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // For each class code 0..15, get the char, convert to string,
    // parse it back, and verify the class bits match.
    let form = r#"(let ((results nil))
                    (dotimes (i 16)
                      (let* ((ch (syntax-class-to-char i))
                             (desc (char-to-string ch))
                             (parsed (string-to-syntax desc)))
                        (when parsed
                          (let ((class-back (logand (car parsed) 65535)))
                            (setq results
                                  (cons (list i ch class-back (= class-back i))
                                        results))))))
                    (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// matching-paren with custom bracket definitions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_matching_paren_custom() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Define custom bracket pairs and verify matching-paren
    let form = r#"(let ((st (make-syntax-table)))
                    ;; Define < > as brackets
                    (modify-syntax-entry ?< "(>" st)
                    (modify-syntax-entry ?> ")<" st)
                    ;; Define { } as brackets
                    (modify-syntax-entry ?{ "(}" st)
                    (modify-syntax-entry ?} "){" st)
                    (with-temp-buffer
                      (set-syntax-table st)
                      (list
                       ;; Standard parens should still work (inherited)
                       (matching-paren ?\()
                       (matching-paren ?\))
                       ;; Custom brackets
                       (matching-paren ?<)
                       (matching-paren ?>)
                       (matching-paren ?{)
                       (matching-paren ?})
                       ;; Non-bracket char returns nil
                       (matching-paren ?a))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// syntax-after with buffer context
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_after_buffer_context() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Insert text, move around, check syntax-after at various positions
    let form = r#"(with-temp-buffer
                    (let ((st (make-syntax-table)))
                      (modify-syntax-entry ?# "<" st)
                      (modify-syntax-entry ?\n ">" st)
                      (modify-syntax-entry ?_ "w" st)
                      (set-syntax-table st)
                      (insert "hello_world # comment\n42")
                      (let ((results nil))
                        ;; Check syntax at each interesting position
                        ;; pos 1: 'h' => word
                        (setq results (cons (syntax-after 1) results))
                        ;; pos 6: '_' => word (we set it)
                        (setq results (cons (syntax-after 6) results))
                        ;; pos 13: '#' => comment-start
                        (setq results (cons (syntax-after 13) results))
                        ;; pos 22: '\n' => comment-end
                        (setq results (cons (syntax-after 22) results))
                        ;; pos 23: '4' => word (digit in standard)
                        (setq results (cons (syntax-after 23) results))
                        (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: custom syntax table for parsing a simple DSL
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_dsl_parsing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a syntax table for a simple config-file DSL:
    //   # starts comments (to end of line)
    //   _ is word constituent (for identifiers like foo_bar)
    //   = is punctuation
    //   " delimits strings
    //   ; is punctuation (statement separator)
    // Then use forward-comment and skip-syntax-forward to tokenize.
    let form = r#"(with-temp-buffer
                    (let ((st (make-syntax-table)))
                      ;; Set up DSL syntax
                      (modify-syntax-entry ?# "<" st)
                      (modify-syntax-entry ?\n ">" st)
                      (modify-syntax-entry ?_ "w" st)
                      (modify-syntax-entry ?= "." st)
                      (modify-syntax-entry ?\; "." st)
                      (modify-syntax-entry ?\" "\"" st)
                      ;; Make letters word-constituent (a-z)
                      (modify-syntax-entry '(?a . ?z) "w" st)
                      (modify-syntax-entry '(?A . ?Z) "w" st)
                      (set-syntax-table st)
                      (insert "key_name = \"value\" ; # comment\nnext_key = 42")
                      (goto-char (point-min))
                      (let ((tokens nil))
                        ;; Token 1: skip whitespace, read word
                        (skip-syntax-forward " ")
                        (let ((start (point)))
                          (skip-syntax-forward "w")
                          (setq tokens (cons (buffer-substring start (point)) tokens)))
                        ;; Token 2: skip whitespace, read punctuation
                        (skip-syntax-forward " ")
                        (let ((start (point)))
                          (skip-syntax-forward ".")
                          (setq tokens (cons (buffer-substring start (point)) tokens)))
                        ;; Token 3: skip whitespace, read string
                        (skip-syntax-forward " ")
                        (let ((start (point)))
                          (forward-sexp 1)
                          (setq tokens (cons (buffer-substring start (point)) tokens)))
                        ;; Skip semicolon and whitespace
                        (skip-syntax-forward " .")
                        ;; Skip comment
                        (forward-comment 1)
                        ;; Token 4: next identifier
                        (skip-syntax-forward " ")
                        (let ((start (point)))
                          (skip-syntax-forward "w")
                          (setq tokens (cons (buffer-substring start (point)) tokens)))
                        (nreverse tokens))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
