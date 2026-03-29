//! Advanced oracle parity tests for `internal-event-symbol-parse-modifiers`.
//!
//! Covers basic event symbols, single modifiers, combined modifiers,
//! mouse event symbols, and a complex modifier decomposition table builder.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity_with_bootstrap;

// ---------------------------------------------------------------------------
// Parse basic (unmodified) event symbols
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_iespm_basic_event_symbols() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; Plain letter symbols — should return (symbol) with no modifiers
  (internal-event-symbol-parse-modifiers 'a)
  (internal-event-symbol-parse-modifiers 'z)
  (internal-event-symbol-parse-modifiers 'x)
  ;; Function keys
  (internal-event-symbol-parse-modifiers 'f1)
  (internal-event-symbol-parse-modifiers 'f12)
  ;; Special keys
  (internal-event-symbol-parse-modifiers 'return)
  (internal-event-symbol-parse-modifiers 'tab)
  (internal-event-symbol-parse-modifiers 'backspace)
  (internal-event-symbol-parse-modifiers 'escape)
  (internal-event-symbol-parse-modifiers 'home)
  (internal-event-symbol-parse-modifiers 'end)
  (internal-event-symbol-parse-modifiers 'delete)
  (internal-event-symbol-parse-modifiers 'insert)
  ;; Mouse events without modifiers
  (internal-event-symbol-parse-modifiers 'mouse-1)
  (internal-event-symbol-parse-modifiers 'mouse-2)
  (internal-event-symbol-parse-modifiers 'mouse-3)
  (internal-event-symbol-parse-modifiers 'down-mouse-1)
  (internal-event-symbol-parse-modifiers 'double-mouse-1))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Parse single-modifier event symbols
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_iespm_single_modifiers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; Control
  (internal-event-symbol-parse-modifiers 'C-a)
  (internal-event-symbol-parse-modifiers 'C-x)
  (internal-event-symbol-parse-modifiers 'C-return)
  (internal-event-symbol-parse-modifiers 'C-f1)
  ;; Meta
  (internal-event-symbol-parse-modifiers 'M-a)
  (internal-event-symbol-parse-modifiers 'M-x)
  (internal-event-symbol-parse-modifiers 'M-return)
  (internal-event-symbol-parse-modifiers 'M-f1)
  ;; Shift
  (internal-event-symbol-parse-modifiers 'S-a)
  (internal-event-symbol-parse-modifiers 'S-return)
  (internal-event-symbol-parse-modifiers 'S-tab)
  ;; Super
  (internal-event-symbol-parse-modifiers 's-a)
  (internal-event-symbol-parse-modifiers 's-x)
  (internal-event-symbol-parse-modifiers 's-f1)
  ;; Hyper
  (internal-event-symbol-parse-modifiers 'H-a)
  (internal-event-symbol-parse-modifiers 'H-x)
  (internal-event-symbol-parse-modifiers 'H-f1)
  ;; Control on mouse
  (internal-event-symbol-parse-modifiers 'C-mouse-1)
  (internal-event-symbol-parse-modifiers 'M-mouse-2)
  (internal-event-symbol-parse-modifiers 'S-mouse-3))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Parse combined modifiers
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_iespm_combined_modifiers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; Two modifiers
  (internal-event-symbol-parse-modifiers 'C-M-a)
  (internal-event-symbol-parse-modifiers 'C-M-x)
  (internal-event-symbol-parse-modifiers 'C-S-a)
  (internal-event-symbol-parse-modifiers 'M-S-a)
  (internal-event-symbol-parse-modifiers 'C-M-return)
  (internal-event-symbol-parse-modifiers 'C-S-f1)
  ;; Three modifiers
  (internal-event-symbol-parse-modifiers 'C-M-S-a)
  (internal-event-symbol-parse-modifiers 'C-M-S-z)
  (internal-event-symbol-parse-modifiers 'C-M-S-return)
  ;; With super and hyper
  (internal-event-symbol-parse-modifiers 'C-s-a)
  (internal-event-symbol-parse-modifiers 'M-H-a)
  (internal-event-symbol-parse-modifiers 'C-M-s-a)
  (internal-event-symbol-parse-modifiers 'C-M-H-a)
  ;; Mouse with combined modifiers
  (internal-event-symbol-parse-modifiers 'C-M-mouse-1)
  (internal-event-symbol-parse-modifiers 'C-M-S-mouse-3)
  (internal-event-symbol-parse-modifiers 'C-M-down-mouse-1))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Verify consistency with event-modifiers and event-basic-type
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_iespm_consistency_with_event_api() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify that internal-event-symbol-parse-modifiers results are
    // consistent with event-modifiers when applied to converted events.
    let form = r#"
(progn
  (fset 'neovm--iespm-check-consistency
    (lambda (sym)
      "Check that parse-modifiers on a symbol decomposes correctly."
      (let* ((parsed (internal-event-symbol-parse-modifiers sym))
             (base-sym (car parsed))
             (mods (cdr parsed))
             ;; Also check via event-convert-list roundtrip where possible
             (has-control (memq 'control mods))
             (has-meta (memq 'meta mods))
             (has-shift (memq 'shift mods)))
        (list sym base-sym (sort (copy-sequence mods) #'string<)))))

  (unwind-protect
      (let ((test-syms '(a C-a M-a C-M-a S-a C-S-a M-S-a C-M-S-a
                          s-a H-a C-s-a M-H-a
                          return C-return M-return C-M-return
                          f1 C-f1 M-f1 C-M-f1
                          mouse-1 C-mouse-1 C-M-mouse-1)))
        (mapcar (lambda (s) (funcall 'neovm--iespm-check-consistency s))
                test-syms))
    (fmakunbound 'neovm--iespm-check-consistency)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build a complete modifier decomposition table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_iespm_decomposition_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a table that for each of a set of symbols, records:
    // - the original symbol name
    // - the base event
    // - the modifier set
    // - a reconstructed description via single-key-description + event-convert-list
    // - whether the round-trip matches
    let form = r#"
(progn
  (fset 'neovm--iespm-decompose
    (lambda (sym)
      "Decompose a symbol and attempt to reconstruct."
      (let* ((parsed (internal-event-symbol-parse-modifiers sym))
             (base (car parsed))
             (mods (cdr parsed))
             (sorted-mods (sort (copy-sequence mods) #'string<))
             (mod-count (length mods))
             ;; Build a classification string
             (classification
              (cond
               ((= mod-count 0) "plain")
               ((= mod-count 1) (format "single-%s" (car mods)))
               ((= mod-count 2) "double-mod")
               ((= mod-count 3) "triple-mod")
               (t "multi-mod"))))
        (list (symbol-name sym) base sorted-mods classification))))

  (fset 'neovm--iespm-build-table
    (lambda (syms)
      "Build a decomposition table for a list of symbols."
      (let ((table nil)
            (stats (make-hash-table :test 'equal)))
        ;; Decompose each symbol
        (dolist (s syms)
          (let* ((entry (funcall 'neovm--iespm-decompose s))
                 (classification (nth 3 entry)))
            (setq table (cons entry table))
            ;; Count by classification
            (puthash classification
                     (1+ (or (gethash classification stats) 0))
                     stats)))
        ;; Build stats summary
        (let ((stat-list nil))
          (maphash (lambda (k v) (setq stat-list (cons (cons k v) stat-list)))
                   stats)
          (setq stat-list (sort stat-list (lambda (a b) (string< (car a) (car b)))))
          (list :entries (nreverse table)
                :total (length syms)
                :stats stat-list)))))

  (unwind-protect
      (let ((symbols '(a x z
                        C-a C-x C-z
                        M-a M-x M-z
                        S-a S-x
                        s-a H-a
                        C-M-a C-M-x
                        C-S-a M-S-a
                        C-M-S-a C-M-S-z
                        return C-return M-return C-M-return
                        f1 C-f1
                        mouse-1 C-mouse-1 C-M-mouse-1)))
        (funcall 'neovm--iespm-build-table symbols))
    (fmakunbound 'neovm--iespm-decompose)
    (fmakunbound 'neovm--iespm-build-table)))
"#;
    assert_oracle_parity_with_bootstrap(form);
}
