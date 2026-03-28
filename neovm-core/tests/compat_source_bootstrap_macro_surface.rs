use neovm_core::emacs_core::load::create_source_bootstrap_context;
use neovm_core::emacs_core::{format_eval_result, parse_forms};

#[test]
fn compat_source_bootstrap_macro_surface_is_minimal() {
    let mut eval = create_source_bootstrap_context();
    eval.set_lexical_binding(true);

    let forms = parse_forms(
        r#"(let ((symbols '(eval-and-compile
                            defvar-local
                            track-mouse
                            with-current-buffer
                            with-temp-buffer
                            with-output-to-string
                            with-syntax-table
                            with-mutex)))
  (mapcar
   (lambda (sym)
     (list sym
           (fboundp sym)
           (macrop sym)))
   symbols))"#,
    )
    .expect("parse");

    let result = eval.eval_expr(&forms[0]);
    let rendered = format_eval_result(&result);
    assert_eq!(
        rendered,
        "OK ((eval-and-compile t t) (defvar-local nil nil) (track-mouse nil nil) (with-current-buffer nil nil) (with-temp-buffer nil nil) (with-output-to-string nil nil) (with-syntax-table nil nil) (with-mutex nil nil))"
    );
}
