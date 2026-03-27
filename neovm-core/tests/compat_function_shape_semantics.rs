mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

#[test]
fn compat_function_shape_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping function shape audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let form = r#"(let ((symbols '(load
                        set-window-buffer
                        select-window
                        window-buffer
                        face-attributes-as-vector
                        switch-to-buffer
                        switch-to-buffer-other-window)))
  (mapcar
   (lambda (sym)
     (let ((fn (symbol-function sym)))
       (list sym
             (functionp fn)
             (if (subrp fn) 'subr 'elisp)
             (autoloadp fn))))
   symbols))"#;

    let gnu = run_oracle_eval(form).expect("GNU Emacs evaluation");
    let neovm = run_neovm_eval(form).expect("NeoVM evaluation");
    assert_eq!(
        neovm, gnu,
        "function shape mismatch:\nGNU: {}\nNeoVM: {}",
        gnu, neovm
    );
}
