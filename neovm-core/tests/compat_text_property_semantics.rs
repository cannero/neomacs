mod common;

use common::{oracle_enabled, run_neovm_eval, run_oracle_eval};

struct TextPropertyCase {
    name: &'static str,
    form: &'static str,
}

#[test]
fn compat_text_property_semantics_matches_gnu_emacs() {
    if !oracle_enabled() {
        eprintln!(
            "skipping text property semantics audit: set NEOVM_FORCE_ORACLE_PATH or place GNU Emacs mirror alongside the repo"
        );
        return;
    }

    let cases = [
        TextPropertyCase {
            name: "string_overlapping_ranges_and_boundary_walk",
            form: r#"(let ((s (copy-sequence "abcdefghij")))
  (put-text-property 0 6 'face 'bold s)
  (put-text-property 3 8 'help-echo "tip" s)
  (list
   (text-properties-at 0 s)
   (text-properties-at 3 s)
   (text-properties-at 7 s)
   (next-property-change 0 s)
   (next-property-change 3 s)
   (next-property-change 6 s)
   (previous-property-change 8 s)
   (previous-property-change 3 s)))"#,
        },
        TextPropertyCase {
            name: "adjacent_identical_properties_merge",
            form: r#"(let ((s (copy-sequence "abcdef")))
  (put-text-property 0 3 'face 'bold s)
  (put-text-property 3 6 'face 'bold s)
  (list
   (next-property-change 0 s)
   (text-properties-at 2 s)
   (text-properties-at 4 s)))"#,
        },
        TextPropertyCase {
            name: "buffer_text_property_boundaries_use_buffer_positions",
            form: r#"(let ((buf (get-buffer-create " *compat-textprop-buffer*")))
  (unwind-protect
      (with-current-buffer buf
        (erase-buffer)
        (insert "abcdefgh")
        (put-text-property 1 5 'face 'bold)
        (put-text-property 5 7 'face 'italic)
        (list
         (text-properties-at 1)
         (text-properties-at 5)
         (next-property-change 1)
         (previous-property-change 7)))
    (kill-buffer buf)))"#,
        },
    ];

    for case in cases {
        let gnu = run_oracle_eval(case.form).expect("GNU Emacs evaluation");
        let neovm = run_neovm_eval(case.form).expect("NeoVM evaluation");
        assert_eq!(
            neovm, gnu,
            "text property semantics mismatch for {}:\nGNU: {}\nNeoVM: {}",
            case.name, gnu, neovm
        );
    }
}
