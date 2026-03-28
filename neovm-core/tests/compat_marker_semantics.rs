mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct MarkerCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_marker_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping marker semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        MarkerCase {
            name: "marker_object_shape_and_print",
            form: r#"(let ((m (make-marker))
      (n (make-marker)))
  (list (markerp m)
        (vectorp m)
        (type-of m)
        (equal m n)
        (prin1-to-string m)))"#,
        },
        MarkerCase {
            name: "marker_survives_buffer_rename",
            form: r#"(let ((buf (get-buffer-create " *compat-marker*"))
      (m (make-marker)))
  (unwind-protect
      (progn
        (with-current-buffer buf
          (erase-buffer)
          (insert "abc")
          (set-marker m 2 buf)
          (rename-buffer " *compat-marker-renamed*" t))
        (list (buffer-name (marker-buffer m))
              (marker-position m)
              (prin1-to-string m)))
    (kill-buffer " *compat-marker-renamed*")))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form)
            .unwrap_or_else(|err| panic!("GNU oracle failed for {}: {err}", case.name));
        let neovm = run_neovm_eval(case.form)
            .unwrap_or_else(|err| panic!("NeoVM eval failed for {}: {err}", case.name));
        assert_eq!(
            neovm, gnu,
            "marker semantics mismatch for {}\nform:\n{}\nneo:\n{}\ngnu:\n{}",
            case.name, case.form, neovm, gnu
        );
    }
}
