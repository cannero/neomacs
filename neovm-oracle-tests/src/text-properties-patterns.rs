//! Oracle parity tests for complex text property patterns:
//! multi-property propertize, put/get roundtrips, selective removal,
//! text-properties-at collection, syntax highlighting simulation,
//! and markup language implementation using text properties.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// propertize with many properties at once, then verify independence
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_propertize_multi_independent() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Attach 6 distinct properties, verify each is independently readable
    // and that modifying one does not affect others
    let form = r#"(let ((s (propertize "abcdef" 'face 'bold
                                        'help-echo "tooltip"
                                        'mouse-face 'highlight
                                        'display '(space :width 10)
                                        'invisible t
                                        'priority 42)))
                   ;; Read all 6 at position 0
                   (let ((before (list (get-text-property 0 'face s)
                                       (get-text-property 0 'help-echo s)
                                       (get-text-property 0 'mouse-face s)
                                       (get-text-property 0 'display s)
                                       (get-text-property 0 'invisible s)
                                       (get-text-property 0 'priority s))))
                     ;; Remove only 'invisible, verify others intact
                     (let ((s2 (copy-sequence s)))
                       (remove-text-properties 0 6 '(invisible nil) s2)
                       (let ((after (list (get-text-property 0 'face s2)
                                          (get-text-property 0 'help-echo s2)
                                          (get-text-property 0 'mouse-face s2)
                                          (get-text-property 0 'display s2)
                                          (get-text-property 0 'invisible s2)
                                          (get-text-property 0 'priority s2))))
                         (list before after
                               ;; Verify property count changed
                               (length (text-properties-at 0 s))
                               (length (text-properties-at 0 s2)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// put-text-property / get-text-property roundtrip across regions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_put_get_roundtrip_regions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Put properties on overlapping regions, then verify precise boundary behavior
    let form = r#"(let ((s (copy-sequence "0123456789ABCDEF")))
                   ;; Region 1: face on 0..8
                   (put-text-property 0 8 'face 'bold s)
                   ;; Region 2: face on 4..12 (overwrites 4..8, extends to 12)
                   (put-text-property 4 12 'face 'italic s)
                   ;; Region 3: category on 6..14 (crosses both regions)
                   (put-text-property 6 14 'category 'alpha s)
                   ;; Collect face+category at every even position
                   (let ((result nil))
                     (let ((i 0))
                       (while (< i 16)
                         (setq result
                               (cons (list i
                                           (get-text-property i 'face s)
                                           (get-text-property i 'category s))
                                     result))
                         (setq i (+ i 2))))
                     (nreverse result)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// remove-text-properties: selective and layered removal
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_remove_selective_layered() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build string with 3 properties, remove them one at a time in different regions
    let form = r#"(let ((s (copy-sequence (propertize "abcdefghij" 'face 'bold
                                                       'help-echo "tip"
                                                       'mouse-face 'highlight))))
                   ;; Remove face from 0..5 only
                   (remove-text-properties 0 5 '(face nil) s)
                   ;; Remove help-echo from 3..8 only
                   (remove-text-properties 3 8 '(help-echo nil) s)
                   ;; Remove mouse-face from 6..10 only
                   (remove-text-properties 6 10 '(mouse-face nil) s)
                   ;; Now survey the property landscape
                   (let ((survey nil))
                     (dotimes (i 10)
                       (setq survey
                             (cons (list i
                                         (get-text-property i 'face s)
                                         (get-text-property i 'help-echo s)
                                         (get-text-property i 'mouse-face s))
                                   survey)))
                     (nreverse survey)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// text-properties-at: exhaustive plist collection at multiple positions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_properties_at_exhaustive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a string with position-dependent properties, collect full plists
    let form = r#"(let ((s (copy-sequence "abcdef")))
                   ;; Each position gets different property combinations
                   (put-text-property 0 1 'a 1 s)
                   (put-text-property 0 2 'b 2 s)
                   (put-text-property 0 3 'c 3 s)
                   (put-text-property 3 4 'd 4 s)
                   (put-text-property 4 6 'e 5 s)
                   (put-text-property 4 6 'f 6 s)
                   ;; Collect property lists and their lengths at each position
                   (let ((result nil))
                     (dotimes (i 6)
                       (let ((plist (text-properties-at i s)))
                         (setq result
                               (cons (list i (length plist) plist) result))))
                     (nreverse result)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Overlay-like syntax highlighting simulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_syntax_highlight_overlay_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate font-lock-like highlighting with priority layering
    let form = r#"(progn
  (fset 'neovm--test-highlight-region
    (lambda (s start end face priority)
      "Apply face with priority, only overwrite if priority is higher."
      (let ((pos start))
        (while (< pos end)
          (let ((cur-priority (or (get-text-property pos 'fontify-priority s) 0)))
            (when (>= priority cur-priority)
              (put-text-property pos (1+ pos) 'face face s)
              (put-text-property pos (1+ pos) 'fontify-priority priority s)))
          (setq pos (1+ pos))))))

  (unwind-protect
      (let ((code (copy-sequence "(defun foo (x) (* x x))")))
        ;; Low priority: everything is default
        (funcall 'neovm--test-highlight-region code 0 (length code)
                 'font-lock-comment-face 0)
        ;; Medium priority: keyword
        (funcall 'neovm--test-highlight-region code 1 6
                 'font-lock-keyword-face 10)
        ;; Medium priority: function name
        (funcall 'neovm--test-highlight-region code 7 10
                 'font-lock-function-name-face 10)
        ;; High priority: operator
        (funcall 'neovm--test-highlight-region code 16 17
                 'font-lock-builtin-face 20)
        ;; Try low priority over keyword region — should NOT overwrite
        (funcall 'neovm--test-highlight-region code 1 6
                 'font-lock-string-face 5)
        ;; Collect face at each position
        (let ((faces nil))
          (dotimes (i (length code))
            (setq faces (cons (cons i (get-text-property i 'face code)) faces)))
          (nreverse faces)))
    (fmakunbound 'neovm--test-highlight-region)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: simple markup language → text properties
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_markup_to_properties() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Parse a simple markup: *bold*, _italic_, `code` and apply text properties
    let form = r#"(progn
  (fset 'neovm--test-apply-markup
    (lambda (input)
      "Convert markup to propertized string. *bold* _italic_ `code`"
      (let ((output "")
            (i 0)
            (len (length input))
            (segments nil))
        ;; Pass 1: collect segments as (text . face)
        (while (< i len)
          (let ((ch (aref input i)))
            (cond
              ;; *bold*
              ((= ch ?*)
               (let ((end (let ((j (1+ i)) (found nil))
                            (while (and (< j len) (not found))
                              (when (= (aref input j) ?*)
                                (setq found j))
                              (setq j (1+ j)))
                            found)))
                 (if end
                     (progn
                       (setq segments (cons (cons (substring input (1+ i) end) 'bold) segments))
                       (setq i (1+ end)))
                   (setq segments (cons (cons (char-to-string ch) nil) segments))
                   (setq i (1+ i)))))
              ;; _italic_
              ((= ch ?_)
               (let ((end (let ((j (1+ i)) (found nil))
                            (while (and (< j len) (not found))
                              (when (= (aref input j) ?_)
                                (setq found j))
                              (setq j (1+ j)))
                            found)))
                 (if end
                     (progn
                       (setq segments (cons (cons (substring input (1+ i) end) 'italic) segments))
                       (setq i (1+ end)))
                   (setq segments (cons (cons (char-to-string ch) nil) segments))
                   (setq i (1+ i)))))
              ;; `code`
              ((= ch ?`)
               (let ((end (let ((j (1+ i)) (found nil))
                            (while (and (< j len) (not found))
                              (when (= (aref input j) ?`)
                                (setq found j))
                              (setq j (1+ j)))
                            found)))
                 (if end
                     (progn
                       (setq segments (cons (cons (substring input (1+ i) end) 'font-lock-constant-face) segments))
                       (setq i (1+ end)))
                   (setq segments (cons (cons (char-to-string ch) nil) segments))
                   (setq i (1+ i)))))
              (t
               (setq segments (cons (cons (char-to-string ch) nil) segments))
               (setq i (1+ i))))))
        ;; Pass 2: build propertized string from reversed segments
        (setq segments (nreverse segments))
        (let ((result ""))
          (dolist (seg segments)
            (let ((text (car seg))
                  (face (cdr seg)))
              (setq result
                    (concat result
                            (if face
                                (propertize text 'face face)
                              text)))))
          result))))

  (unwind-protect
      (let ((result (funcall 'neovm--test-apply-markup
                             "Hello *world* and _universe_ with `code`!")))
        (list
          ;; Plain text content
          (substring-no-properties result)
          ;; Length
          (length result)
          ;; Face at "world" (should be bold)
          (get-text-property 6 'face result)
          ;; Face at "universe" (should be italic)
          (get-text-property 16 'face result)
          ;; Face at "code" (should be font-lock-constant-face)
          (get-text-property 30 'face result)
          ;; Face at "Hello" (should be nil)
          (get-text-property 0 'face result)
          ;; Collect all (pos . face) at boundaries
          (let ((boundaries nil)
                (pos 0))
            (while (< pos (length result))
              (let ((face (get-text-property pos 'face result)))
                (setq boundaries (cons (cons pos face) boundaries)))
              (setq pos (or (next-property-change pos result) (length result))))
            (nreverse boundaries))))
    (fmakunbound 'neovm--test-apply-markup)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// set-text-properties: wholesale replacement
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_set_text_properties_wholesale() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // set-text-properties replaces ALL properties in a region
    let form = r#"(let ((s (copy-sequence (propertize "abcdefgh" 'face 'bold
                                                       'help-echo "old"
                                                       'mouse-face 'highlight))))
                   ;; Replace all properties in 2..6 with a completely new set
                   (set-text-properties 2 6 '(category test-cat priority 99) s)
                   ;; Verify: positions 0-1 retain original, 2-5 have new, 6-7 retain original
                   (list
                     ;; Original region preserved
                     (get-text-property 0 'face s)
                     (get-text-property 0 'help-echo s)
                     (get-text-property 1 'mouse-face s)
                     ;; Replaced region: old props gone, new props present
                     (get-text-property 2 'face s)
                     (get-text-property 2 'help-echo s)
                     (get-text-property 2 'category s)
                     (get-text-property 2 'priority s)
                     (get-text-property 5 'category s)
                     ;; Post-replaced region: original restored
                     (get-text-property 6 'face s)
                     (get-text-property 6 'help-echo s)
                     ;; Property counts
                     (length (text-properties-at 0 s))
                     (length (text-properties-at 3 s))
                     (length (text-properties-at 7 s))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// add-text-properties vs put-text-property: additive vs overwriting
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_add_vs_put_semantics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // add-text-properties adds without removing existing; put overwrites the specific key
    let form = r#"(let ((s1 (copy-sequence (propertize "test" 'a 1 'b 2)))
                        (s2 (copy-sequence (propertize "test" 'a 1 'b 2))))
                   ;; add-text-properties: add 'c, change 'a
                   (add-text-properties 0 4 '(a 10 c 3) s1)
                   ;; put-text-property: change 'a only (b remains)
                   (put-text-property 0 4 'a 10 s2)
                   (list
                     ;; s1: a=10 (changed), b=2 (kept), c=3 (added)
                     (get-text-property 0 'a s1)
                     (get-text-property 0 'b s1)
                     (get-text-property 0 'c s1)
                     ;; s2: a=10 (changed), b=2 (kept), c=nil (not added)
                     (get-text-property 0 'a s2)
                     (get-text-property 0 'b s2)
                     (get-text-property 0 'c s2)
                     ;; Property counts
                     (length (text-properties-at 0 s1))
                     (length (text-properties-at 0 s2))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: property-based word frequency annotator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_tpp_word_frequency_annotator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Count word frequencies, then annotate each occurrence with its frequency
    let form = r#"(progn
  (fset 'neovm--test-annotate-freqs
    (lambda (text)
      (let ((words (split-string text " " t))
            (freq-table (make-hash-table :test 'equal)))
        ;; Count frequencies
        (dolist (w words)
          (puthash w (1+ (gethash w freq-table 0)) freq-table))
        ;; Build annotated string
        (let ((result (copy-sequence text))
              (pos 0))
          (dolist (w words)
            ;; Find word start in result
            (let ((start (let ((idx pos) (found nil))
                           (while (and (< idx (length result)) (not found))
                             (when (and (string= w (substring result idx
                                                              (min (+ idx (length w)) (length result))))
                                        (or (= idx 0) (= (aref result (1- idx)) ? ))
                                        (or (= (+ idx (length w)) (length result))
                                            (= (aref result (+ idx (length w))) ? )))
                               (setq found idx))
                             (setq idx (1+ idx)))
                           found)))
              (when start
                (put-text-property start (+ start (length w))
                                   'word-freq (gethash w freq-table) result)
                (setq pos (+ start (length w))))))
          result))))

  (unwind-protect
      (let ((annotated (funcall 'neovm--test-annotate-freqs
                                "the cat sat on the mat the cat")))
        (list
          (substring-no-properties annotated)
          ;; "the" appears 3 times
          (get-text-property 0 'word-freq annotated)
          ;; "cat" appears 2 times
          (get-text-property 4 'word-freq annotated)
          ;; "sat" appears 1 time
          (get-text-property 8 'word-freq annotated)
          ;; "on" appears 1 time
          (get-text-property 12 'word-freq annotated)
          ;; second "the"
          (get-text-property 15 'word-freq annotated)))
    (fmakunbound 'neovm--test-annotate-freqs)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
