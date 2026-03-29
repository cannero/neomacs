//! Oracle parity tests for type predicates: `booleanp`, `characterp`,
//! `functionp`, `keywordp`, `nlistp`, `string-or-null-p`,
//! `integer-or-null-p`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// booleanp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_booleanp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(booleanp t)");
    assert_oracle_parity_with_bootstrap("(booleanp nil)");
    assert_oracle_parity_with_bootstrap("(booleanp 0)");
    assert_oracle_parity_with_bootstrap("(booleanp 1)");
    assert_oracle_parity_with_bootstrap("(booleanp 'hello)");
    assert_oracle_parity_with_bootstrap("(booleanp '())");
}

#[test]
fn oracle_prop_booleanp_expressions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Results of predicates should be booleanp
    let form = "(list (booleanp (= 1 1))
                      (booleanp (null nil))
                      (booleanp (not 42)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// characterp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_characterp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(characterp ?a)");
    assert_oracle_parity_with_bootstrap("(characterp ?Z)");
    assert_oracle_parity_with_bootstrap("(characterp ?\\n)");
    assert_oracle_parity_with_bootstrap("(characterp 65)");
    assert_oracle_parity_with_bootstrap("(characterp 0)");
    assert_oracle_parity_with_bootstrap("(characterp -1)");
    assert_oracle_parity_with_bootstrap("(characterp nil)");
    assert_oracle_parity_with_bootstrap(r#"(characterp "a")"#);
    assert_oracle_parity_with_bootstrap("(characterp 'a)");
}

#[test]
fn oracle_prop_characterp_large_codepoint() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Max valid Unicode codepoint
    assert_oracle_parity_with_bootstrap("(characterp #x10ffff)");
    // Beyond max
    assert_oracle_parity_with_bootstrap("(characterp #x110000)");
}

// ---------------------------------------------------------------------------
// functionp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_functionp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(functionp 'car)");
    assert_oracle_parity_with_bootstrap("(functionp (lambda (x) x))");
    assert_oracle_parity_with_bootstrap("(functionp #'car)");
    assert_oracle_parity_with_bootstrap("(functionp nil)");
    assert_oracle_parity_with_bootstrap("(functionp 42)");
    assert_oracle_parity_with_bootstrap("(functionp '(1 2 3))");
    assert_oracle_parity_with_bootstrap(r#"(functionp "hello")"#);
}

#[test]
fn oracle_prop_functionp_closures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 10))
                  (let ((f (lambda () x)))
                    (functionp f)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

// ---------------------------------------------------------------------------
// keywordp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_keywordp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(keywordp :test)");
    assert_oracle_parity_with_bootstrap("(keywordp :hello)");
    assert_oracle_parity_with_bootstrap("(keywordp :)");
    assert_oracle_parity_with_bootstrap("(keywordp 'test)");
    assert_oracle_parity_with_bootstrap("(keywordp nil)");
    assert_oracle_parity_with_bootstrap("(keywordp 42)");
    assert_oracle_parity_with_bootstrap(r#"(keywordp ":test")"#);
}

// ---------------------------------------------------------------------------
// nlistp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nlistp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(nlistp nil)");
    assert_oracle_parity_with_bootstrap("(nlistp '(1 2 3))");
    assert_oracle_parity_with_bootstrap("(nlistp '(a . b))");
    assert_oracle_parity_with_bootstrap("(nlistp 42)");
    assert_oracle_parity_with_bootstrap(r#"(nlistp "hello")"#);
    assert_oracle_parity_with_bootstrap("(nlistp [1 2 3])");
    assert_oracle_parity_with_bootstrap("(nlistp t)");
}

// ---------------------------------------------------------------------------
// string-or-null-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_or_null_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(string-or-null-p "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(string-or-null-p "")"#);
    assert_oracle_parity_with_bootstrap("(string-or-null-p nil)");
    assert_oracle_parity_with_bootstrap("(string-or-null-p 42)");
    assert_oracle_parity_with_bootstrap("(string-or-null-p 'hello)");
    assert_oracle_parity_with_bootstrap("(string-or-null-p t)");
}

// ---------------------------------------------------------------------------
// integer-or-null-p (called integer-or-marker-p in some contexts)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_integer_or_null_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(integerp 42)");
    assert_oracle_parity_with_bootstrap("(integerp 0)");
    assert_oracle_parity_with_bootstrap("(integerp -7)");
    assert_oracle_parity_with_bootstrap("(integerp nil)");
    assert_oracle_parity_with_bootstrap("(integerp 3.14)");
    assert_oracle_parity_with_bootstrap(r#"(integerp "42")"#);
}

// ---------------------------------------------------------------------------
// Complex: type dispatch pattern
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_type_dispatch_complex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement a generic "describe" function using type predicates
    let form = r####"(let ((describe
                     (lambda (val)
                       (cond
                         ((null val) "null")
                         ((booleanp val) "boolean")
                         ((integerp val) (format "int:%d" val))
                         ((floatp val) "float")
                         ((stringp val)
                          (format "str:%d" (length val)))
                         ((keywordp val) "keyword")
                         ((symbolp val) "symbol")
                         ((functionp val) "function")
                         ((vectorp val)
                          (format "vec:%d" (length val)))
                         ((consp val)
                          (format "cons:%d" (length val)))
                         (t "unknown")))))
                    (list (funcall describe nil)
                          (funcall describe t)
                          (funcall describe 42)
                          (funcall describe 3.14)
                          (funcall describe "hello")
                          (funcall describe :test)
                          (funcall describe 'foo)
                          (funcall describe (lambda () nil))
                          (funcall describe [1 2 3])
                          (funcall describe '(a b c))))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_type_coercion_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Type-safe conversion pipeline
    let form = r####"(let ((to-string
                     (lambda (val)
                       (cond
                         ((stringp val) val)
                         ((numberp val) (number-to-string val))
                         ((symbolp val) (symbol-name val))
                         ((null val) "nil")
                         (t (prin1-to-string val))))))
                    (let ((values '(42 3.14 hello nil "already" (1 2) [3 4])))
                      (mapcar to-string values)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_type_predicate_exhaustive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Every value should match exactly one primary type
    let form = r####"(let ((classify
                     (lambda (val)
                       (let ((types nil))
                         (when (integerp val) (setq types (cons 'integer types)))
                         (when (floatp val) (setq types (cons 'float types)))
                         (when (stringp val) (setq types (cons 'string types)))
                         (when (symbolp val) (setq types (cons 'symbol types)))
                         (when (consp val) (setq types (cons 'cons types)))
                         (when (vectorp val) (setq types (cons 'vector types)))
                         types))))
                    (list (funcall classify 42)
                          (funcall classify 3.14)
                          (funcall classify "hi")
                          (funcall classify 'foo)
                          (funcall classify '(1 2))
                          (funcall classify [1 2])
                          (funcall classify nil)
                          (funcall classify t)))"####;
    assert_oracle_parity_with_bootstrap(form);
}
