//! Oracle parity tests for `string-match-p` (non-modifying match).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

#[test]
fn oracle_prop_string_match_p_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "foo" "foobar")"#,
    );
    assert_ok_eq("0", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "bar" "foobar")"#,
    );
    assert_ok_eq("3", &o, &n);
}

#[test]
fn oracle_prop_string_match_p_no_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "xyz" "foobar")"#,
    );
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_string_match_p_with_start() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Start searching from position 3
    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "o" "foobar" 2)"#,
    );
    assert_ok_eq("2", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "o" "foobar" 3)"#,
    );
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_string_match_p_regex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "^[0-9]+$" "12345")"#,
    );
    assert_ok_eq("0", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-match-p "^[0-9]+$" "123abc")"#,
    );
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_string_match_p_does_not_modify_match_data() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string-match-p should NOT modify match data
    let form = r#"(progn
                    (string-match "\\(foo\\)" "foobar")
                    (let ((before (match-beginning 1)))
                      (string-match-p "bar" "xyzbar")
                      (let ((after (match-beginning 1)))
                        (list before after (= before after)))))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_string_match_p_character_classes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-match-p "[[:alpha:]]+" "hello")"#);
    assert_oracle_parity(r#"(string-match-p "[[:digit:]]+" "abc123")"#);
    assert_oracle_parity(r#"(string-match-p "[[:space:]]" "hello world")"#);
}

#[test]
fn oracle_prop_string_match_p_in_conditional() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Common pattern: use string-match-p as a predicate
    let form = r#"(mapcar (lambda (s)
                            (if (string-match-p "^test-" s) 'test 'other))
                          '("test-foo" "hello" "test-bar" "world"))"#;
    assert_oracle_parity(form);
}
