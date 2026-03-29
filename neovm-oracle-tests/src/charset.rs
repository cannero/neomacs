//! Oracle parity tests for charset primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_charset_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list (char-charset ?A) (charsetp (char-charset ?A)) (encode-char ?A 'ucs) (decode-char 'ucs #x41) (encode-char ?😀 'ucs) (decode-char 'ucs #x1F600))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(ascii t 65 65 128512 128512)", &oracle, &neovm);
}

#[test]
fn oracle_prop_char_charset_classification() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form =
        "(list (char-charset ?A) (char-charset ?é) (char-charset ?😀) (char-charset ?\\x80))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(ascii unicode-bmp unicode unicode-bmp)", &oracle, &neovm);
}

#[test]
fn oracle_prop_encode_char_unknown_charset_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(encode-char ?A 'neovm-no-such-charset)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_decode_char_out_of_range_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(decode-char 'ucs -1)");
}
