mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct BufferLocalsCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_buffer_locals_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping buffer locals audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        BufferLocalsCase {
            name: "buffer_local_value_and_void_binding",
            form: r#"(let ((buf (get-buffer-create " *compat-buffer-local-value*")))
  (unwind-protect
      (progn
        (set-buffer buf)
        (make-local-variable 'compat-void-local)
        (make-local-variable 'compat-bound-local)
        (setq compat-bound-local 42)
        (set-default 'compat-default-local 9)
        (list
         (condition-case err
             (buffer-local-value 'compat-void-local buf)
           (error (car err)))
         (buffer-local-value 'compat-bound-local buf)
         (buffer-local-value 'compat-default-local buf)))
    (kill-buffer buf)))"#,
        },
        BufferLocalsCase {
            name: "buffer_local_variables_shape",
            form: r#"(let ((buf (get-buffer-create " *compat-buffer-local-vars*")))
  (unwind-protect
      (progn
        (set-buffer buf)
        (make-local-variable 'compat-void-local)
        (make-local-variable 'compat-bound-local)
        (setq compat-bound-local 42)
        (let ((locals (buffer-local-variables buf)))
          (list
           (assq 'compat-bound-local locals)
           (memq 'compat-void-local locals)
           (assq 'major-mode locals)
           (assq 'buffer-read-only locals)
           (assq 'buffer-undo-list locals))))
    (kill-buffer buf)))"#,
        },
        BufferLocalsCase {
            name: "kill_all_local_variables_resets_current_buffer",
            form: r#"(let ((buf (get-buffer-create " *compat-kill-all-locals*")))
  (unwind-protect
      (progn
        (set-buffer buf)
        (make-local-variable 'compat-bound-local)
        (setq compat-bound-local 42)
        (setq mode-name "Manual")
        (setq major-mode 'compat-major)
        (kill-all-local-variables)
        (list
         (condition-case err compat-bound-local (error (car err)))
         major-mode
         mode-name
         (local-variable-p 'compat-bound-local buf)))
    (kill-buffer buf)))"#,
        },
        BufferLocalsCase {
            name: "make_indirect_buffer_clone_metadata",
            form: r#"(let ((base (get-buffer-create " *compat-indirect-base*")))
  (unwind-protect
      (progn
        (set-buffer base)
        (insert "hello")
        (setq mode-name "Base")
        (setq-local compat-local 7)
        (let ((indirect (make-indirect-buffer base " *compat-indirect-copy*" t)))
          (unwind-protect
              (progn
                (list
                 (buffer-base-buffer indirect)
                 (buffer-local-value 'mode-name indirect)
                 (buffer-local-value 'compat-local indirect)
                 (with-current-buffer indirect (buffer-string))))
            (kill-buffer indirect))))
    (kill-buffer base)))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "buffer locals semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
