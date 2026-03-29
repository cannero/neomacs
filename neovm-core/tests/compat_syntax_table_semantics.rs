mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct SyntaxCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_syntax_table_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping syntax-table audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        SyntaxCase {
            name: "syntax_after_uses_syntax_table_text_properties",
            form: r#"(with-temp-buffer
  (insert "ab")
  (put-text-property 2 3 'syntax-table (string-to-syntax " "))
  (list
   (equal (syntax-after 2) (string-to-syntax " "))
   (char-syntax (char-after 2))))"#,
        },
        SyntaxCase {
            name: "scan_sexps_honors_parse_sexp_lookup_properties",
            form: r#"(with-temp-buffer
  (insert "x@y@z")
  (put-text-property 2 3 'syntax-table (string-to-syntax "|"))
  (put-text-property 4 5 'syntax-table (string-to-syntax "|"))
  (list
   (let ((parse-sexp-lookup-properties nil))
     (condition-case err
         (scan-sexps 2 1)
       (error (list 'error (car err)))))
   (let ((parse-sexp-lookup-properties t))
     (condition-case err
         (scan-sexps 2 1)
       (error (list 'error (car err)))))))"#,
        },
        SyntaxCase {
            name: "forward_comment_honors_syntax_table_text_properties",
            form: r##"(with-temp-buffer
  (insert "#hi#x")
  (put-text-property 1 2 'syntax-table (string-to-syntax "!"))
  (put-text-property 4 5 'syntax-table (string-to-syntax "!"))
  (list
   (let ((parse-sexp-lookup-properties nil))
     (goto-char 1)
     (list (forward-comment 1) (point)))
   (let ((parse-sexp-lookup-properties t))
     (goto-char 1)
     (list (forward-comment 1) (point)))))"##,
        },
        SyntaxCase {
            name: "backward_prefix_chars_uses_prefix_class_and_properties",
            form: r#"(list
  (with-temp-buffer
    (insert "'x")
    (goto-char 2)
    (backward-prefix-chars)
    (point))
  (with-temp-buffer
    (insert "+x")
    (put-text-property 1 2 'syntax-table (string-to-syntax "'"))
    (list
     (let ((parse-sexp-lookup-properties nil))
       (goto-char 2)
       (backward-prefix-chars)
       (point))
     (let ((parse-sexp-lookup-properties t))
       (goto-char 2)
       (backward-prefix-chars)
       (point)))))"#,
        },
    ];

    for case in cases {
        eprintln!("syntax-table case: {}", case.name);
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "syntax-table semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
