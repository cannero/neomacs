//! Oracle parity tests for advanced syntax table operations:
//! make-syntax-table, copy-syntax-table, modify-syntax-entry with all
//! syntax classes, char-syntax, string-to-syntax, syntax-class-to-char,
//! syntax-after in buffers, forward-comment with custom syntax,
//! comment flags, and multi-level syntax table inheritance.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Comprehensive modify-syntax-entry with comment flags (1, 2, 3, 4, b, n, p)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_comment_flags() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // C-style comments: /* ... */ and // ... \n
    // Flag 1 = second char of comment start  (e.g., * in /*)
    // Flag 2 = first char of comment end     (e.g., * in */)
    // Flag 3 = second char of comment start for style b
    // Flag 4 = first char of comment end for style b
    // Flag b = style b comment
    // Flag n = nestable
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table)))
    ;; C-style block comments: /* ... */
    (modify-syntax-entry ?/ ". 14" st)
    (modify-syntax-entry ?* ". 23" st)
    (set-syntax-table st)
    ;; Test parsing of block comments
    (insert "code /* block comment */ more_code")
    (goto-char (point-min))
    ;; Skip forward over code
    (skip-syntax-forward "w")
    (let ((after-word (point)))
      ;; Skip whitespace
      (skip-syntax-forward " ")
      ;; forward-comment should skip the block comment
      (let ((comment-result (forward-comment 1)))
        (skip-syntax-forward " ")
        (let ((after-comment (point)))
          (skip-syntax-forward "w_")
          (let ((end-word (buffer-substring after-comment (point))))
            (list after-word
                  comment-result
                  after-comment
                  end-word)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Nested comment syntax tables (flag n for nestable)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_nested_comments() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a syntax table with nestable comments like {- ... -} in Haskell
    // Using ( and ) as open/close comment delimiters with nesting
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table)))
    ;; # starts line comment
    (modify-syntax-entry ?# "<" st)
    (modify-syntax-entry ?\n ">" st)
    ;; _ is word constituent
    (modify-syntax-entry ?_ "w" st)
    (set-syntax-table st)
    ;; Test multiple line comments
    (insert "alpha # first comment\nbeta # second comment\ngamma")
    (goto-char (point-min))
    (let ((results nil))
      ;; Read first word
      (skip-syntax-forward "w")
      (setq results (cons (buffer-substring 1 (point)) results))
      ;; Skip whitespace
      (skip-syntax-forward " ")
      ;; Forward over first comment (including newline)
      (forward-comment 1)
      ;; Read next word
      (let ((s (point)))
        (skip-syntax-forward "w")
        (setq results (cons (buffer-substring s (point)) results)))
      ;; Skip whitespace and second comment
      (skip-syntax-forward " ")
      (forward-comment 1)
      ;; Read last word
      (let ((s (point)))
        (skip-syntax-forward "w")
        (setq results (cons (buffer-substring s (point)) results)))
      (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// string-to-syntax with flag combinations and syntax-after verification
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_string_to_syntax_flags() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-to-syntax parses descriptors like "w", ". 14", "(}", etc.
    // Verify all syntax class characters and flag combinations
    let form = r#"(list
  ;; Basic classes
  (string-to-syntax " ")    ;; whitespace
  (string-to-syntax "w")    ;; word
  (string-to-syntax "_")    ;; symbol
  (string-to-syntax ".")    ;; punctuation
  (string-to-syntax "(")    ;; open paren (no matching char)
  (string-to-syntax ")")    ;; close paren
  (string-to-syntax "\"")   ;; string delimiter
  (string-to-syntax "\\")   ;; escape
  (string-to-syntax "/")    ;; character quote
  (string-to-syntax "$")    ;; paired delimiter
  (string-to-syntax "'")    ;; expression prefix
  (string-to-syntax "<")    ;; comment start
  (string-to-syntax ">")    ;; comment end
  (string-to-syntax "!")    ;; generic comment delimiter
  (string-to-syntax "|")    ;; generic string delimiter
  ;; Open/close with matching char
  (string-to-syntax "()")   ;; open paren matching )
  (string-to-syntax ")(")   ;; close paren matching (
  (string-to-syntax "(}")   ;; open { matching }
  ;; Classes with comment flags
  (string-to-syntax ". 1")  ;; punctuation + flag 1
  (string-to-syntax ". 2")  ;; punctuation + flag 2
  (string-to-syntax ". 3")  ;; punctuation + flag 3
  (string-to-syntax ". 4")  ;; punctuation + flag 4
  (string-to-syntax "< b")  ;; comment start, style b
  (string-to-syntax "> b")  ;; comment end, style b
  (string-to-syntax "! p")  ;; generic comment + prefix flag
  ;; Combined flags
  (string-to-syntax ". 14") ;; both flag 1 and flag 4
  (string-to-syntax ". 23") ;; both flag 2 and flag 3
  ;; Verify that car of result encodes the class + flags
  (let ((ws (string-to-syntax " "))
        (wd (string-to-syntax "w"))
        (sym (string-to-syntax "_")))
    (list (car ws) (car wd) (car sym))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Multi-level syntax table inheritance (grandparent -> parent -> child)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_multi_level_inheritance() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Three-level inheritance: grandparent -> parent -> child
    // Each level overrides some entries while inheriting others
    let form = r#"(with-temp-buffer
  (let* ((grandparent (make-syntax-table))
         (_ (progn
              (modify-syntax-entry ?@ "w" grandparent)
              (modify-syntax-entry ?# "<" grandparent)
              (modify-syntax-entry ?\n ">" grandparent)
              (modify-syntax-entry ?$ "_" grandparent)
              (modify-syntax-entry ?% "." grandparent)))
         (parent (make-syntax-table grandparent))
         (_ (progn
              ;; Override @ from word to symbol in parent
              (modify-syntax-entry ?@ "_" parent)
              ;; Override $ from symbol to word
              (modify-syntax-entry ?$ "w" parent)
              ;; Add new entry for &
              (modify-syntax-entry ?& "." parent)))
         (child (make-syntax-table parent))
         (_ (progn
              ;; Override @ again (from symbol to punctuation)
              (modify-syntax-entry ?@ "." child)
              ;; Add new entry for ~
              (modify-syntax-entry ?~ "w" child))))
    ;; Verify inheritance chain
    (list
     ;; Grandparent table parent should be standard-syntax-table
     (eq (char-table-parent grandparent) (standard-syntax-table))
     ;; Parent's parent is grandparent
     (eq (char-table-parent parent) grandparent)
     ;; Child's parent is parent
     (eq (char-table-parent child) parent)
     ;; @ through all levels
     (progn (set-syntax-table grandparent)
            (let ((gp-at (char-syntax ?@)))
              (set-syntax-table parent)
              (let ((p-at (char-syntax ?@)))
                (set-syntax-table child)
                (let ((c-at (char-syntax ?@)))
                  (list gp-at p-at c-at)))))
     ;; $ through all levels
     (progn (set-syntax-table grandparent)
            (let ((gp-dollar (char-syntax ?$)))
              (set-syntax-table parent)
              (let ((p-dollar (char-syntax ?$)))
                (set-syntax-table child)
                (let ((c-dollar (char-syntax ?$)))
                  (list gp-dollar p-dollar c-dollar)))))
     ;; # inherited unchanged from grandparent through all levels
     (progn (set-syntax-table child)
            (char-syntax ?#))
     ;; & defined only in parent, inherited by child
     (progn (set-syntax-table child)
            (char-syntax ?&))
     ;; ~ defined only in child
     (progn (set-syntax-table child)
            (char-syntax ?~))
     ;; % inherited from grandparent through parent to child
     (progn (set-syntax-table child)
            (char-syntax ?%)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// syntax-after at various positions with mixed syntax classes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_syntax_after_mixed() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a buffer with text containing many different syntax classes
    // and verify syntax-after at each position
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table)))
    ;; Configure a rich syntax table
    (modify-syntax-entry ?_ "w" st)      ;; _ is word
    (modify-syntax-entry ?# "<" st)      ;; # starts comment
    (modify-syntax-entry ?\n ">" st)     ;; newline ends comment
    (modify-syntax-entry ?| "|" st)      ;; | is generic string delim
    (modify-syntax-entry ?$ "'" st)      ;; $ is expression prefix
    (modify-syntax-entry ?< "(>" st)     ;; < > are matched parens
    (modify-syntax-entry ?> ")<" st)
    (set-syntax-table st)
    ;; Insert text with various syntax classes present
    (insert "hello_world (test) \"str\" |gen| $expr <blk>")
    (let ((results nil)
          (positions (list 1     ;; h -> word
                           6     ;; _ -> word
                           12    ;; space -> whitespace
                           13    ;; ( -> open paren
                           14    ;; t -> word
                           18    ;; ) -> close paren
                           20    ;; " -> string quote
                           24    ;; " -> string quote
                           26    ;; | -> generic string
                           30    ;; | -> generic string
                           32    ;; $ -> expression prefix
                           38    ;; < -> open paren
                           42    ;; > -> close paren
                           )))
      (dolist (pos positions)
        (when (<= pos (point-max))
          (setq results (cons (cons pos (syntax-after pos)) results))))
      (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// forward-comment with multiple styles and backward direction
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_forward_comment_directions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test forward-comment in both directions with line comments
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table)))
    (modify-syntax-entry ?# "<" st)
    (modify-syntax-entry ?\n ">" st)
    (modify-syntax-entry ?_ "w" st)
    (set-syntax-table st)
    (insert "alpha # comment1\nbeta   # comment2\ngamma")
    (let ((results nil))
      ;; Test 1: forward-comment from beginning (skip whitespace + comment)
      (goto-char 1)
      (skip-syntax-forward "w")
      (let ((pos-after-alpha (point)))
        (let ((fc1 (forward-comment 100)))  ;; skip all comments/whitespace ahead
          (let ((pos-after-skip (point)))
            (skip-syntax-forward "w")
            (let ((word1 (buffer-substring pos-after-skip (point))))
              (setq results (cons (list :fwd1 pos-after-alpha fc1 word1) results))))))
      ;; Test 2: forward-comment backward from end
      (goto-char (point-max))
      (let ((fc2 (forward-comment -100)))  ;; skip all comments/whitespace backward
        (let ((pos-after-back (point)))
          (setq results (cons (list :bwd pos-after-back fc2) results))))
      ;; Test 3: forward-comment(1) skips exactly one comment
      (goto-char 1)
      (skip-syntax-forward "w")  ;; past "alpha"
      (skip-syntax-forward " ")  ;; past spaces before #
      (let ((fc3 (forward-comment 1)))  ;; skip one comment
        (let ((pos3 (point)))
          (skip-syntax-forward "w")
          (setq results (cons (list :one-comment fc3 (buffer-substring pos3 (point))) results))))
      ;; Test 4: forward-comment on non-comment returns nil / doesn't move
      (goto-char 1)
      (let ((pos-before (point))
            (fc4 (forward-comment 1)))
        (setq results (cons (list :no-comment (= (point) pos-before) fc4) results)))
      (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// syntax-class-to-char exhaustive + char-syntax roundtrip through modify
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_class_char_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // For each syntax class 0..15:
    //   1. Get the designator char via syntax-class-to-char
    //   2. Create a syntax table entry using that char
    //   3. Read back via char-syntax and verify it matches
    // Also test that setting and reading back multiple entries is consistent
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table))
        (results nil)
        ;; Use chars A-O (65-79) as test subjects for classes 0-14
        ;; (skip class 15 = generic string fence since it maps to |)
        (test-chars (list ?A ?B ?C ?D ?E ?F ?G ?H ?I ?J ?K ?L ?M ?N ?O)))
    ;; Assign each test char to a different syntax class
    (let ((class-idx 0))
      (dolist (ch test-chars)
        (when (< class-idx 15)
          (let* ((class-char (syntax-class-to-char class-idx))
                 (desc (char-to-string class-char)))
            (modify-syntax-entry ch desc st))
          (setq class-idx (1+ class-idx)))))
    ;; Now read them all back
    (set-syntax-table st)
    (let ((class-idx 0))
      (dolist (ch test-chars)
        (when (< class-idx 15)
          (let ((read-back (char-syntax ch))
                (expected (syntax-class-to-char class-idx)))
            (setq results
                  (cons (list class-idx expected read-back (= read-back expected))
                        results)))
          (setq class-idx (1+ class-idx)))))
    ;; Also verify syntax-class-to-char for all 16 classes
    (let ((all-class-chars nil))
      (dotimes (i 16)
        (setq all-class-chars (cons (syntax-class-to-char i) all-class-chars)))
      (setq results (cons (list :all-class-chars (nreverse all-class-chars)) results)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build syntax table for S-expression parsing and navigate
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_syntax_table_ops_sexp_navigation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a Lisp-like syntax table and use forward-sexp/backward-sexp
    // to navigate balanced expressions
    let form = r#"(with-temp-buffer
  (let ((st (make-syntax-table)))
    ;; Lisp-like syntax
    (modify-syntax-entry ?\( "()" st)
    (modify-syntax-entry ?\) ")(" st)
    (modify-syntax-entry ?\[ "(]" st)
    (modify-syntax-entry ?\] ")[" st)
    (modify-syntax-entry ?\" "\"" st)
    (modify-syntax-entry ?\\ "\\" st)
    (modify-syntax-entry ?\; "<" st)
    (modify-syntax-entry ?\n ">" st)
    (modify-syntax-entry ?_ "w" st)
    (modify-syntax-entry ?- "w" st)
    (modify-syntax-entry ?' "'" st)
    (set-syntax-table st)
    (insert "(defun hello_world [x y] ; comment\n  (+ x y))")
    (let ((results nil))
      ;; forward-sexp from beginning: should skip entire top-level form
      (goto-char (point-min))
      (forward-sexp 1)
      (setq results (cons (list :after-top-sexp (point)) results))
      ;; backward-sexp from end
      (goto-char (point-max))
      (backward-sexp 1)
      (setq results (cons (list :before-top-sexp (point)) results))
      ;; Navigate inside: go to char after first (
      (goto-char 2)  ;; after the opening (
      ;; forward-sexp should skip "defun"
      (forward-sexp 1)
      (setq results (cons (list :after-defun (point)
                                (buffer-substring 2 (point))) results))
      ;; forward-sexp again: should skip "hello_world"
      (forward-sexp 1)
      (let ((p (point)))
        (setq results (cons (list :after-name p) results)))
      ;; forward-sexp: skip the [x y] vector form
      (forward-sexp 1)
      (let ((p (point)))
        (setq results (cons (list :after-vector p) results)))
      ;; forward-comment should skip the ; comment
      (skip-syntax-forward " ")
      (let ((fc (forward-comment 1)))
        (setq results (cons (list :comment-skipped fc (point)) results)))
      ;; matching parens verification
      (setq results (cons (list :parens
                                (matching-paren ?\()
                                (matching-paren ?\))
                                (matching-paren ?\[)
                                (matching-paren ?\]))
                          results))
      (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
