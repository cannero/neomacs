//! Oracle parity tests for `indent-to`.

use super::common::assert_oracle_parity_with_bootstrap;

#[test]
fn oracle_indent_to_respects_tab_width_and_indent_tabs_mode() {
    let form = r#"(list
                    (let ((tab-width 4) (indent-tabs-mode t))
                      (with-temp-buffer
                        (list (indent-to 6 1)
                              (current-column)
                              (append (buffer-string) nil))))
                    (let ((tab-width 4) (indent-tabs-mode nil))
                      (with-temp-buffer
                        (list (indent-to 6 1)
                              (current-column)
                              (append (buffer-string) nil))))
                    (with-temp-buffer
                      (setq tab-width 4)
                      (insert "ab")
                      (list (indent-to 6 2)
                            (current-column)
                            (append (buffer-string) nil))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
