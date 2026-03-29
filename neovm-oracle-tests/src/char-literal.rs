//! Oracle parity tests for character-literal parsing (`?x`, `?\M-x`, etc.).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_char_literal_modifier_bits() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list ?\M-a ?\C-a ?\M-\C-a ?\S-a)"#;
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(134217825 1 134217729 33554529)", &oracle, &neovm);
}

#[test]
fn oracle_prop_char_literal_unicode_codepoints() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(list ?😀 ?𐌀)");
    assert_ok_eq("(128512 66304)", &oracle, &neovm);
}
