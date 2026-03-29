//! Advanced oracle parity tests for `move-to-column` patterns.
//!
//! Covers: FORCE argument (nil vs t), tab characters and tab-stop behavior,
//! column vs character position differences, lines shorter than target column,
//! FORCE creating spaces, combined with `current-column`, interactive
//! column tracking, and practical column-based editing operations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// move-to-column with tabs: column landing inside tab stops
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_tab_stop_interactions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Tabs expand to next tab stop. move-to-column without FORCE lands at
    // the nearest reachable column. Test all positions within a tab span.
    let form = r#"(with-temp-buffer
  (insert "\tABC\tDEF\n")
  ;; With tab-width=8: Tab -> col 8, A=8, B=9, C=10, Tab -> col 16, D=16 ...
  (let ((results nil))
    ;; Try move-to-column for cols 0 through 20 without FORCE
    (let ((col-map nil))
      (dotimes (target 21)
        (goto-char (point-min))
        (let ((ret (move-to-column target)))
          (setq col-map (cons (list target ret (current-column) (point)) col-map))))
      (setq results (cons (cons 'no-force (nreverse col-map)) results)))
    ;; Now with FORCE=t: tabs should be split into spaces when needed
    (let ((force-results nil))
      ;; Specifically target columns inside the first tab (cols 1-7)
      (dolist (target '(1 3 5 7))
        (with-temp-buffer
          (insert "\tABC\n")
          (goto-char (point-min))
          (let ((ret (move-to-column target t)))
            (setq force-results
                  (cons (list target ret (current-column) (point)
                              (buffer-string))
                        force-results)))))
      (setq results (cons (cons 'force-split (nreverse force-results)) results)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// move-to-column FORCE: padding short lines with spaces
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_force_pad_short_lines() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // When FORCE is non-nil and the line is shorter than the target column,
    // spaces are inserted to reach the target. Verify buffer modification,
    // returned value, and that other lines are not affected.
    let form = r#"(let ((results nil))
  ;; Pad multiple lines of varying lengths to column 20
  (with-temp-buffer
    (insert "ab\ndefgh\n\nxyz01234567890\n")
    (let ((before (buffer-string)))
      ;; Line 1: "ab" (len 2), pad to col 20
      (goto-char (point-min))
      (let ((r (move-to-column 20 t)))
        (setq results (cons (list 'line1 r (current-column)
                                  (buffer-substring (line-beginning-position)
                                                    (line-end-position)))
                            results)))
      ;; Line 2: "defgh" (len 5), pad to col 20
      (forward-line 1)
      (beginning-of-line)
      (let ((r (move-to-column 20 t)))
        (setq results (cons (list 'line2 r (current-column)
                                  (buffer-substring (line-beginning-position)
                                                    (line-end-position)))
                            results)))
      ;; Line 3: "" (empty), pad to col 10
      (forward-line 1)
      (beginning-of-line)
      (let ((r (move-to-column 10 t)))
        (setq results (cons (list 'line3-empty r (current-column)
                                  (buffer-substring (line-beginning-position)
                                                    (line-end-position)))
                            results)))
      ;; Line 4: already long enough, no padding needed
      (forward-line 1)
      (beginning-of-line)
      (let ((before-line (buffer-substring (line-beginning-position)
                                           (line-end-position)))
            (r (move-to-column 8 t)))
        (setq results (cons (list 'line4-nopad r (current-column)
                                  (string= before-line
                                           (buffer-substring (line-beginning-position)
                                                             (line-end-position))))
                            results)))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// move-to-column: column vs character position with mixed content
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_col_vs_char_position() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Demonstrate that column number differs from character position when
    // tabs and control characters are present. Build a detailed mapping.
    let form = r#"(with-temp-buffer
  ;; Tab-width 8. Content: "a\tb\t\tc" where \t is tab
  ;; a=col0, tab->col8, b=col8, tab->col16, tab->col24, c=col24
  (insert "a\tb\t\tc\n")
  ;; Also a line with control chars: ^A displays as 2 cols
  (insert (concat (string 1) "X" (string 2) "Y\n"))
  (let ((results nil))
    ;; Line 1: map character positions to columns
    (goto-char (point-min))
    (let ((char-col-map nil))
      (while (not (eolp))
        (setq char-col-map (cons (list (point)
                                       (following-char)
                                       (current-column))
                                 char-col-map))
        (forward-char 1))
      (setq results (cons (cons 'line1-map (nreverse char-col-map)) results)))
    ;; Line 1: move-to-column for specific columns and record char position
    (let ((col-pos-map nil))
      (dolist (col '(0 4 8 9 12 16 20 24 25))
        (goto-char (point-min))
        (move-to-column col)
        (setq col-pos-map (cons (list col (current-column) (point))
                                col-pos-map)))
      (setq results (cons (cons 'line1-col-to-pos (nreverse col-pos-map))
                          results)))
    ;; Line 2: control characters widen the column count
    (forward-line 1)
    (beginning-of-line)
    (let ((ctrl-map nil))
      (while (not (eolp))
        (setq ctrl-map (cons (list (point) (following-char) (current-column))
                             ctrl-map))
        (forward-char 1))
      (setq results (cons (cons 'line2-ctrl (nreverse ctrl-map)) results)))
    ;; Verify that for plain ASCII, column = (point - line-beginning-position)
    (with-temp-buffer
      (insert "abcdefghij\n")
      (goto-char (point-min))
      (let ((all-match t))
        (while (not (eolp))
          (unless (= (current-column) (- (point) (line-beginning-position)))
            (setq all-match nil))
          (forward-char 1))
        (setq results (cons (list 'plain-ascii-match all-match) results))))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// move-to-column with different tab-width values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_different_tab_widths() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test move-to-column behavior with tab-width 2, 4, and 8 on the
    // same tab-containing text.
    let form = r#"(let ((results nil))
  (dolist (tw '(2 4 8))
    (with-temp-buffer
      (setq tab-width tw)
      (insert "\tA\tB\n")
      ;; Map columns for this tab-width
      (goto-char (point-min))
      (let ((map nil))
        (while (not (eolp))
          (setq map (cons (list (following-char) (current-column)) map))
          (forward-char 1))
        (setq results (cons (list 'tw tw (nreverse map)) results)))
      ;; move-to-column targets within first tab
      (let ((mtc-results nil))
        (dotimes (i (1+ (* tw 3)))
          (goto-char (point-min))
          (let ((ret (move-to-column i)))
            (setq mtc-results (cons (list i ret (current-column)) mtc-results))))
        (setq results (cons (list 'mtc tw (nreverse mtc-results)) results)))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Practical: column-based rectangle operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_rectangle_extract() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use move-to-column to extract a "rectangle" of text (columns C1..C2)
    // from each line, simulating rectangle-kill.
    let form = r#"(progn
  (fset 'neovm--test-extract-rect
    (lambda (start-col end-col)
      "Extract text from columns START-COL to END-COL on each line."
      (save-excursion
        (goto-char (point-min))
        (let ((rect nil))
          (while (not (eobp))
            (let (p1 p2)
              (move-to-column start-col)
              (setq p1 (point))
              (move-to-column end-col)
              (setq p2 (point))
              (setq rect (cons (buffer-substring p1 p2) rect)))
            (forward-line 1))
          (nreverse rect)))))
  (unwind-protect
      (with-temp-buffer
        (insert "Name        Age  City\n")
        (insert "Alice       30   NYC\n")
        (insert "Bob         25   SF\n")
        (insert "Charlemagne 45   LA\n")
        (insert "Di          22   Chicago\n")
        (list
          ;; Extract Name column (cols 0-11)
          (funcall 'neovm--test-extract-rect 0 12)
          ;; Extract Age column (cols 12-16)
          (funcall 'neovm--test-extract-rect 12 17)
          ;; Extract City column (cols 17-30)
          (funcall 'neovm--test-extract-rect 17 30)
          ;; Extract beyond line end (should get partial/empty strings)
          (funcall 'neovm--test-extract-rect 20 40)))
    (fmakunbound 'neovm--test-extract-rect)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// move-to-column with FORCE and subsequent insert
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_force_then_insert() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use FORCE to reach a column, then insert text there. Verify the
    // resulting buffer content and column positions.
    let form = r#"(with-temp-buffer
  (insert "line1\nline2\nline3\n")
  (let ((results nil))
    ;; Insert "|" at column 10 on each line using FORCE
    (goto-char (point-min))
    (let ((n 0))
      (while (not (eobp))
        (move-to-column 10 t)
        (insert "|")
        (setq n (1+ n))
        (forward-line 1)))
    (setq results (cons (list 'after-insert (buffer-string)) results))
    ;; Verify column 10 has "|" on each line
    (goto-char (point-min))
    (let ((checks nil))
      (while (not (eobp))
        (move-to-column 10)
        (setq checks (cons (list (line-number-at-pos)
                                 (current-column)
                                 (char-after))
                           checks))
        (forward-line 1))
      (setq results (cons (cons 'checks (nreverse checks)) results)))
    ;; Another test: FORCE with tab-containing lines
    (erase-buffer)
    (insert "\tAAA\n")
    (insert "BB\tCCC\n")
    ;; Insert marker at column 5 with FORCE (splits the tab)
    (goto-char (point-min))
    (move-to-column 5 t)
    (insert "*")
    (setq results (cons (list 'tab-split-1 (buffer-string)) results))
    ;; Line 2: "BB\tCCC" - col 5 is inside the tab after "BB"
    (forward-line 1)
    (beginning-of-line)
    (move-to-column 5 t)
    (insert "*")
    (setq results (cons (list 'tab-split-2 (buffer-string)) results))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Column tracking through edits: insert, delete, and verify
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_track_through_edits() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Track how current-column changes as we insert and delete characters,
    // and verify move-to-column finds the right position after edits.
    let form = r#"(with-temp-buffer
  (insert "0123456789\n")
  (let ((results nil))
    ;; Initial: move to col 5, record
    (goto-char (point-min))
    (move-to-column 5)
    (setq results (cons (list 'initial (current-column) (point) (char-after))
                        results))
    ;; Insert 3 chars at col 3 -> col 5 now points to what was col 2
    (goto-char (point-min))
    (move-to-column 3)
    (insert "XXX")
    ;; Now move to col 5 again
    (goto-char (point-min))
    (move-to-column 5)
    (setq results (cons (list 'after-insert (current-column) (point) (char-after)
                              (buffer-substring (line-beginning-position)
                                                (line-end-position)))
                        results))
    ;; Delete from col 1 to col 4 (3 chars of "0XX")
    (goto-char (point-min))
    (move-to-column 1)
    (let ((del-start (point)))
      (move-to-column 4)
      (delete-region del-start (point)))
    (setq results (cons (list 'after-delete
                              (buffer-substring (line-beginning-position)
                                                (line-end-position)))
                        results))
    ;; Verify: move to every column and record what char is there
    (goto-char (point-min))
    (let ((col-chars nil)
          (line-str (buffer-substring (line-beginning-position)
                                      (line-end-position))))
      (dotimes (c (1+ (length line-str)))
        (goto-char (point-min))
        (move-to-column c)
        (setq col-chars (cons (list c (current-column)
                                    (if (eolp) 'eol (char-after)))
                              col-chars)))
      (setq results (cons (cons 'final-map (nreverse col-chars)) results)))
    (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// move-to-column: comprehensive return value semantics
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_move_to_column_return_value_semantics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // The return value of move-to-column is the column actually reached.
    // Verify this carefully for: exact match, overshoot (tab), undershoot
    // (short line), FORCE cases.
    let form = r#"(let ((results nil))
  ;; Case 1: exact match (plain text)
  (with-temp-buffer
    (insert "abcdefghij\n")
    (goto-char (point-min))
    (setq results (cons (list 'exact (move-to-column 5) (current-column))
                        results)))
  ;; Case 2: tab overshoot (target inside tab)
  (with-temp-buffer
    (insert "\tX\n")
    (goto-char (point-min))
    ;; Tab goes from col 0 to col 8. Target col 4 cannot be reached exactly.
    (let ((ret (move-to-column 4)))
      (setq results (cons (list 'tab-overshoot ret (current-column) (point))
                          results))))
  ;; Case 3: line too short, no FORCE
  (with-temp-buffer
    (insert "abc\n")
    (goto-char (point-min))
    (let ((ret (move-to-column 10)))
      (setq results (cons (list 'short-no-force ret (current-column) (point))
                          results))))
  ;; Case 4: line too short, with FORCE
  (with-temp-buffer
    (insert "abc\n")
    (goto-char (point-min))
    (let ((ret (move-to-column 10 t)))
      (setq results (cons (list 'short-force ret (current-column) (point)
                                (buffer-string))
                          results))))
  ;; Case 5: FORCE splitting tab -> return exact column
  (with-temp-buffer
    (insert "\tX\n")
    (goto-char (point-min))
    (let ((ret (move-to-column 4 t)))
      (setq results (cons (list 'force-tab-split ret (current-column)
                                (buffer-string))
                          results))))
  ;; Case 6: column 0 always succeeds
  (with-temp-buffer
    (insert "anything\n")
    (goto-char (point-min))
    (forward-char 5)
    (let ((ret (move-to-column 0)))
      (setq results (cons (list 'col-zero ret (current-column) (point))
                          results))))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}
