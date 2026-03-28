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

    let form = r#"(let ((symbols '(if
                        throw
                        load
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

#[test]
fn compat_public_evaluator_subr_masking_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping public evaluator subr masking audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let form = r#"(let ((before
         (list
          (list 'if (fboundp 'if) (special-form-p 'if)
                (let ((fn (symbol-function 'if)))
                  (list (subrp fn) (autoloadp fn))))
          (list 'throw (fboundp 'throw) (special-form-p 'throw)
                (let ((fn (symbol-function 'throw)))
                  (list (subrp fn) (autoloadp fn))))))
      (saved-if (symbol-function 'if))
      (saved-throw (symbol-function 'throw)))
  (unwind-protect
      (progn
        (fmakunbound 'if)
        (fmakunbound 'throw)
        (list before
              (list 'if (fboundp 'if) (symbol-function 'if)
                    (condition-case err (if t 1 2) (error (car err))))
              (list 'throw (fboundp 'throw) (symbol-function 'throw)
                    (condition-case err (throw 'tag 1) (error (car err))))))
    (fset 'if saved-if)
    (fset 'throw saved-throw)))"#;

    let gnu = run_oracle_eval(form).expect("GNU Emacs evaluation");
    let neovm = run_neovm_eval(form).expect("NeoVM evaluation");
    assert_eq!(
        neovm, gnu,
        "public evaluator subr masking mismatch:\nGNU: {}\nNeoVM: {}",
        gnu, neovm
    );
}
