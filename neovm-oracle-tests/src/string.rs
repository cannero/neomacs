//! Oracle parity tests for string primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_stringp_wrong_arity_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(stringp)");
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

#[test]
fn oracle_prop_concat_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(concat "a" 1)"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_substring_out_of_range_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(substring "abc" 10)"#);
    assert_err_kind(&oracle, &neovm, "args-out-of-range");
}

#[test]
fn oracle_prop_string_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(string "a")"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_stringp_operator(
        s in proptest::string::string_regex(r"[A-Za-z0-9 _-]{0,24}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(stringp {:?})", s);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("t", &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_concat_operator(
        a in proptest::string::string_regex(r"[A-Za-z0-9 _-]{0,16}").expect("regex should compile"),
        b in proptest::string::string_regex(r"[A-Za-z0-9 _-]{0,16}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(concat {:?} {:?})", a, b);
        let expected = format!("{:?}", format!("{a}{b}"));
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_substring_operator(
        len in 0usize..24usize,
        start in 0usize..24usize,
        end in 0usize..24usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));
        prop_assume!(start <= end && end <= len);

        let source = "a".repeat(len);
        let form = format!("(substring {:?} {} {})", source, start, end);
        let expected = format!("{:?}", &source[start..end]);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_length_string_operator(
        s in proptest::string::string_regex(r"[A-Za-z0-9 _-]{0,24}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let expected_len = s.len();
        let form = format!("(length {:?})", s);
        let expected = expected_len.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_string_operator(
        chars in prop::collection::vec(97u8..123u8, 0usize..24usize),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let args = chars
            .iter()
            .map(|c| (*c as i64).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let form = if args.is_empty() {
            "(string)".to_string()
        } else {
            format!("(string {args})")
        };

        let expected_string: String = chars.iter().map(|c| char::from(*c)).collect();
        let expected = format!("{expected_string:?}");
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
