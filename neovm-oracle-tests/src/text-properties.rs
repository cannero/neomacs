//! Oracle parity tests for text properties.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_propertize_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        r#"(get-text-property 0 'face (propertize "hello" 'face 'bold))"#,
    );
}

#[test]
fn oracle_prop_put_text_property_and_get() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(let ((s (copy-sequence "hello")))
                    (put-text-property 0 3 'face 'italic s)
                    (list (get-text-property 0 'face s)
                          (get-text-property 3 'face s)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_text_properties_at() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(text-properties-at 0 (propertize "hi" 'a 1 'b 2))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_next_property_change() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(let ((s (concat (propertize "abc" 'face 'bold) "def")))
                    (next-property-change 0 s))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_propertize_multiple_props() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(let ((s (propertize "test" 'face 'bold 'help-echo "tip")))
                    (list (get-text-property 0 'face s)
                          (get-text-property 0 'help-echo s)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_remove_text_properties() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(let ((s (propertize "hello" 'face 'bold)))
                    (remove-text-properties 0 5 '(face nil) s)
                    (get-text-property 0 'face s))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_buffer_text_properties() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert (propertize "hello" 'face 'bold))
                    (get-text-property 1 'face))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_propertize_preserves_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(string-equal "hello" (propertize "hello" 'face 'bold))"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}
