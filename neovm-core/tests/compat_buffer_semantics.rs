mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct BufferCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_buffer_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping buffer semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        BufferCase {
            name: "modified_and_restore_transitions",
            form: r#"(let ((buf (get-buffer-create " *compat-buffer-state*")))
  (unwind-protect
      (progn
        (set-buffer buf)
        (list :initial
              (buffer-modified-p)
              (buffer-modified-tick)
              (buffer-chars-modified-tick)
              (recent-auto-save-p)
              :after-set-t
              (progn
                (set-buffer-modified-p t)
                (list (buffer-modified-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)
                      (recent-auto-save-p)))
              :after-restore-nil
              (progn
                (restore-buffer-modified-p nil)
                (list (buffer-modified-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)
                      (recent-auto-save-p)))
              :after-restore-autosaved
              (progn
                (restore-buffer-modified-p 'autosaved)
                (list (buffer-modified-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)
                      (recent-auto-save-p)))))
    (kill-buffer buf)))"#,
        },
        BufferCase {
            name: "autosave_state_transitions",
            form: r#"(let ((buf (get-buffer-create " *compat-buffer-auto*")))
  (unwind-protect
      (progn
        (set-buffer buf)
        (insert "x")
        (list :before-auto
              (buffer-modified-p)
              (recent-auto-save-p)
              (buffer-modified-tick)
              (buffer-chars-modified-tick)
              :after-auto
              (progn
                (set-buffer-auto-saved)
                (list (buffer-modified-p)
                      (recent-auto-save-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)))
              :after-set-t
              (progn
                (set-buffer-modified-p t)
                (list (buffer-modified-p)
                      (recent-auto-save-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)))
              :after-insert
              (progn
                (insert "y")
                (list (buffer-modified-p)
                      (recent-auto-save-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)))
              :after-clear
              (progn
                (set-buffer-modified-p nil)
                (list (buffer-modified-p)
                      (recent-auto-save-p)
                      (buffer-modified-tick)
                      (buffer-chars-modified-tick)))))
    (kill-buffer buf)))"#,
        },
        BufferCase {
            name: "indirect_buffer_text_properties_follow_shared_text_edits",
            form: r#"(let ((base (get-buffer-create " *compat-buffer-text-props-base*")))
  (unwind-protect
      (progn
        (with-current-buffer base
          (erase-buffer)
          (insert "abcdef")
          (put-text-property 2 5 'face 'bold))
        (let ((indirect
               (make-indirect-buffer base " *compat-buffer-text-props-indirect*" nil)))
          (unwind-protect
              (progn
                (with-current-buffer indirect
                  (delete-region 3 4))
                (list
                 (with-current-buffer base
                   (list
                    (buffer-string)
                    (get-text-property 2 'face)
                    (get-text-property 3 'face)
                    (get-text-property 4 'face)
                    (get-text-property 5 'face)))
                 (with-current-buffer indirect
                   (list
                    (buffer-string)
                    (get-text-property 2 'face)
                    (get-text-property 3 'face)
                    (get-text-property 4 'face)
                    (get-text-property 5 'face)))))
            (kill-buffer indirect))))
    (kill-buffer base)))"#,
        },
        BufferCase {
            name: "indirect_buffer_undo_list_follows_shared_text_history",
            form: r#"(let ((base (get-buffer-create " *compat-buffer-undo-base*")))
  (unwind-protect
      (progn
        (with-current-buffer base
          (erase-buffer)
          (setq buffer-undo-list nil)
          (insert "abc"))
        (let ((indirect
               (make-indirect-buffer base " *compat-buffer-undo-indirect*" nil)))
          (unwind-protect
              (list
               (with-current-buffer base
                 (prin1-to-string buffer-undo-list))
               (with-current-buffer indirect
                 (prin1-to-string buffer-undo-list))
               (with-current-buffer indirect
                 (let ((buffer-undo-list buffer-undo-list))
                   (primitive-undo 1 buffer-undo-list)
                   (buffer-string)))
               (with-current-buffer base
                 (buffer-string)))
            (kill-buffer indirect))))
    (kill-buffer base)))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "buffer semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
