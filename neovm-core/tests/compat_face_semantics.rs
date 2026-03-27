mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct FaceCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_face_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping face semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        FaceCase {
            name: "face_attributes_as_vector_permissive_merge",
            form: r#"(list
  (face-attributes-as-vector nil)
  (face-attributes-as-vector '(:foreground "red" :background "blue" :weight bold :underline (:style wave :color "red") :box t :extend t))
  (face-attributes-as-vector '(:bogus 1 :foreground "red" :font foo :inherit bar :fontset baz :stipple qux))
  (face-attributes-as-vector 1))"#,
        },
        FaceCase {
            name: "face_attributes_as_vector_nil_and_box_cases",
            form: r#"(list
  (face-attributes-as-vector '(:underline nil :overline nil :strike-through nil :box nil))
  (face-attributes-as-vector '(:box "red"))
  (face-attributes-as-vector '(:box (:line-width 3 :color "red" :style released-button)))
  (face-attributes-as-vector '(:foreground nil :background nil :distant-foreground nil :extend nil)))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "face semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
