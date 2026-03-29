mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct KeymapCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_keymap_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping keymap semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        KeymapCase {
            name: "keymap_text_property_precedes_minor_local_and_global_maps",
            form: r#"(progn
  (defvar demo-mode nil)
  (let ((g (make-sparse-keymap))
      (l (make-sparse-keymap))
      (m (make-sparse-keymap))
      (tp (make-sparse-keymap))
      (minor-mode-map-alist nil))
  (unwind-protect
      (with-temp-buffer
        (use-global-map g)
        (use-local-map l)
        (define-key g "a" 'global-a)
        (define-key l "a" 'local-a)
        (define-key m "a" 'minor-a)
        (define-key tp "a" 'textprop-a)
        (setq demo-mode t)
        (setq minor-mode-map-alist (list (cons 'demo-mode m)))
        (insert "ab")
        (put-text-property 1 2 'keymap tp)
        (goto-char 1)
        (list
         (mapcar (lambda (map) (lookup-key map "a" t))
                 (current-active-maps nil 1))
         (key-binding "a" t nil 1)
         (key-binding "a" t nil (copy-marker 1))))
    (setq minor-mode-map-alist nil))))"#,
        },
        KeymapCase {
            name: "local_map_text_property_replaces_buffer_local_map_only_at_position",
            form: r#"(progn
  (defvar demo-mode nil)
  (let ((g (make-sparse-keymap))
      (l (make-sparse-keymap))
      (m (make-sparse-keymap))
      (lp (make-sparse-keymap))
      (minor-mode-map-alist nil))
  (unwind-protect
      (with-temp-buffer
        (use-global-map g)
        (use-local-map l)
        (define-key g "a" 'global-a)
        (define-key l "a" 'local-a)
        (define-key m "a" 'minor-a)
        (define-key lp "a" 'property-local-a)
        (setq demo-mode t)
        (setq minor-mode-map-alist (list (cons 'demo-mode m)))
        (insert "ab")
        (put-text-property 1 2 'local-map lp)
        (goto-char 1)
        (list
         (mapcar (lambda (map) (lookup-key map "a" t))
                 (current-active-maps nil 1))
         (mapcar (lambda (map) (lookup-key map "a" t))
                 (current-active-maps nil 2))
         (key-binding "a" t nil 1)
         (key-binding "a" t nil 2)))
    (setq minor-mode-map-alist nil))))"#,
        },
        KeymapCase {
            name: "overriding_local_map_suppresses_text_property_minor_and_local_maps",
            form: r#"(progn
  (defvar demo-mode nil)
  (let ((g (make-sparse-keymap))
      (l (make-sparse-keymap))
      (m (make-sparse-keymap))
      (tp (make-sparse-keymap))
      (ov (make-sparse-keymap))
      (minor-mode-map-alist nil)
      (overriding-local-map nil))
  (with-temp-buffer
    (use-global-map g)
    (use-local-map l)
    (define-key g "a" 'global-a)
    (define-key l "a" 'local-a)
    (define-key m "a" 'minor-a)
    (define-key tp "a" 'textprop-a)
    (define-key ov "a" 'override-a)
    (setq demo-mode t)
    (setq minor-mode-map-alist (list (cons 'demo-mode m)))
    (insert "ab")
    (put-text-property 1 2 'keymap tp)
    (goto-char 1)
    (setq overriding-local-map ov)
    (list
     (mapcar (lambda (map) (lookup-key map "a" t))
             (current-active-maps t 1))
     (key-binding "a" t nil 1)))))"#,
        },
        KeymapCase {
            name: "overriding_terminal_local_map_precedes_all_other_active_maps",
            form: r#"(progn
  (defvar demo-mode nil)
  (let ((g (make-sparse-keymap))
      (l (make-sparse-keymap))
      (m (make-sparse-keymap))
      (tp (make-sparse-keymap))
      (term (make-sparse-keymap))
      (minor-mode-map-alist nil)
      (overriding-terminal-local-map nil))
  (with-temp-buffer
    (use-global-map g)
    (use-local-map l)
    (define-key g "a" 'global-a)
    (define-key l "a" 'local-a)
    (define-key m "a" 'minor-a)
    (define-key tp "a" 'textprop-a)
    (define-key term "a" 'terminal-override-a)
    (setq demo-mode t)
    (setq minor-mode-map-alist (list (cons 'demo-mode m)))
    (insert "ab")
    (put-text-property 1 2 'keymap tp)
    (goto-char 1)
    (setq overriding-terminal-local-map term)
    (list
     (mapcar (lambda (map) (lookup-key map "a" t))
             (current-active-maps t 1))
     (key-binding "a" t nil 1)))))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "keymap semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
