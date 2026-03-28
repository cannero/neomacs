mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct BufferSwitchCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_buffer_switch_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping buffer switch semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        BufferSwitchCase {
            name: "noncurrent_indirect_point_and_narrowing_track_base_insert",
            form: r#"(let ((base (get-buffer-create " *compat-switch-base*")))
  (unwind-protect
      (progn
        (with-current-buffer base
          (erase-buffer)
          (insert "abcdef"))
        (let ((indirect
               (make-indirect-buffer base " *compat-switch-indirect*" nil)))
          (unwind-protect
              (progn
                (with-current-buffer indirect
                  (narrow-to-region 2 5)
                  (goto-char 4))
                (with-current-buffer base
                  (goto-char 1)
                  (insert "ZZ"))
                (list
                 (with-current-buffer base
                   (list (point) (point-min) (point-max) (buffer-string)))
                 (with-current-buffer indirect
                   (list (point) (point-min) (point-max) (buffer-string)))))
            (kill-buffer indirect))))
    (kill-buffer base)))"#,
        },
        BufferSwitchCase {
            name: "noncurrent_indirect_point_and_narrowing_track_base_delete",
            form: r#"(let ((base (get-buffer-create " *compat-switch-delete-base*")))
  (unwind-protect
      (progn
        (with-current-buffer base
          (erase-buffer)
          (insert "abcdefgh"))
        (let ((indirect
               (make-indirect-buffer base " *compat-switch-delete-indirect*" nil)))
          (unwind-protect
              (progn
                (with-current-buffer indirect
                  (narrow-to-region 3 7)
                  (goto-char 6))
                (with-current-buffer base
                  (delete-region 2 5))
                (list
                 (with-current-buffer base
                   (list (point) (point-min) (point-max) (buffer-string)))
                 (with-current-buffer indirect
                   (list (point) (point-min) (point-max) (buffer-string)))))
            (kill-buffer indirect))))
    (kill-buffer base)))"#,
        },
        BufferSwitchCase {
            name: "clone_indirect_buffer_preserves_state_after_switching_away_and_back",
            form: r#"(let ((base (get-buffer-create " *compat-switch-clone-base*")))
  (unwind-protect
      (progn
        (with-current-buffer base
          (erase-buffer)
          (insert "abcdefgh")
          (narrow-to-region 2 7)
          (goto-char 5))
        (let ((clone
               (make-indirect-buffer base " *compat-switch-clone*" t)))
          (unwind-protect
              (progn
                (with-current-buffer base
                  (goto-char 3))
                (with-current-buffer clone
                  (goto-char 6))
                (set-buffer base)
                (set-buffer clone)
                (list
                 (with-current-buffer base
                   (list (point) (point-min) (point-max)))
                 (with-current-buffer clone
                   (list (point) (point-min) (point-max)))))
            (kill-buffer clone))))
    (kill-buffer base)))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "buffer switch semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
