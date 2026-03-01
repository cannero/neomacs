//! Oracle parity tests for advanced `eval` usage:
//! eval with lexical environments, eval of dynamically constructed
//! forms, eval in macroexpand patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// eval basic forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_eval_quoted_forms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(eval '(+ 1 2 3))");
    assert_oracle_parity("(eval '(list 1 2 3))");
    assert_oracle_parity("(eval ''hello)");
    assert_oracle_parity("(eval 42)");
    assert_oracle_parity(r#"(eval "hello")"#);
}

#[test]
fn oracle_prop_eval_constructed_forms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Dynamically construct and evaluate forms
    let form = "(let ((op '+)
                      (args '(1 2 3 4 5)))
                  (eval (cons op args)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &o, &n);
}

#[test]
fn oracle_prop_eval_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(eval (eval '(quote (+ 1 2))))");
}

// ---------------------------------------------------------------------------
// eval with let-constructed forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_eval_dynamic_let() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a let form dynamically and eval it
    let form = "(let ((bindings '((x 10) (y 20)))
                      (body '(+ x y)))
                  (eval (list 'let bindings body)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("30", &o, &n);
}

#[test]
fn oracle_prop_eval_dynamic_cond() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a cond form dynamically
    let form = "(let ((clauses (list (list '(= 1 2) 'branch-a)
                                    (list '(= 2 2) 'branch-b)
                                    (list t 'default))))
                  (eval (cons 'cond clauses)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("branch-b", &o, &n);
}

// ---------------------------------------------------------------------------
// eval in metaprogramming patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_eval_code_generation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Generate and evaluate code
    let form = "(let ((gen-adder
                       (lambda (n)
                         (list 'lambda '(x) (list '+ 'x n)))))
                  (let ((add5 (eval (funcall gen-adder 5)))
                        (add10 (eval (funcall gen-adder 10))))
                    (list (funcall add5 3)
                          (funcall add10 3))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_eval_template_expansion() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Backquote-like template expansion via eval
    let form = "(let ((make-checker
                       (lambda (field value)
                         (list 'lambda '(record)
                               (list 'equal
                                     (list 'cdr
                                           (list 'assq
                                                 (list 'quote field)
                                                 'record))
                                     (list 'quote value))))))
                  (let ((is-alice (eval (funcall make-checker
                                                 'name 'alice)))
                        (is-bob (eval (funcall make-checker
                                               'name 'bob))))
                    (let ((record '((name . alice) (age . 30))))
                      (list (funcall is-alice record)
                            (funcall is-bob record)))))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// eval with progn and multiple forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_eval_progn_forms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((forms '((setq neovm--test-eval-tmp 0)
                                (setq neovm--test-eval-tmp
                                      (1+ neovm--test-eval-tmp))
                                (setq neovm--test-eval-tmp
                                      (1+ neovm--test-eval-tmp))
                                neovm--test-eval-tmp)))
                  (eval (cons 'progn forms)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("2", &o, &n);
}

// ---------------------------------------------------------------------------
// Complex: mini test framework using eval
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_eval_test_framework() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple assertion framework
    let form = "(let ((tests '(((= (+ 1 2) 3) . \"1+2=3\")
                                ((= (* 3 4) 12) . \"3*4=12\")
                                ((= (- 10 7) 3) . \"10-7=3\")
                                ((string= (concat \"a\" \"b\") \"ab\")
                                 . \"concat\")
                                ((= (length '(1 2 3)) 3)
                                 . \"length\")))
                      (passed 0) (failed 0) (failures nil))
                  (dolist (test tests)
                    (if (eval (car test))
                        (setq passed (1+ passed))
                      (setq failed (1+ failed)
                            failures (cons (cdr test) failures))))
                  (list passed failed (nreverse failures)))";
    assert_oracle_parity(form);
}
