//! Comprehensive oracle parity tests for `apply` and `funcall`:
//! spread arguments, empty final lists, single list argument, lambda/closure/subr
//! targets, &rest and &optional parameters, nested chains, higher-order
//! function results as arguments, and error edge cases.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// apply with spread arguments before final list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_spread_args_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 0 spread args, just a list
    assert_oracle_parity_with_bootstrap("(apply #'+ '(10 20 30))");
    // 1 spread arg before list
    assert_oracle_parity_with_bootstrap("(apply #'+ 100 '(1 2 3))");
    // 2 spread args before list
    assert_oracle_parity_with_bootstrap("(apply #'* 2 3 '(4 5))");
    // 3 spread args before list
    assert_oracle_parity_with_bootstrap("(apply #'list 'a 'b 'c '(d e f))");
    // 5 spread args before list
    assert_oracle_parity_with_bootstrap("(apply #'+ 1 2 3 4 5 '(6 7 8 9 10))");
    // Spread args with string concat
    assert_oracle_parity_with_bootstrap(r#"(apply #'concat "hello" " " '("world" "!"))"#);
    // Nested list construction via spread + final list
    assert_oracle_parity_with_bootstrap("(apply #'list '(1 2) '(3 4) '((5 6) (7 8)))");
    // Mixed types in spread args
    assert_oracle_parity_with_bootstrap(r#"(apply #'list 42 "str" 'sym ?A '(3.14 nil t))"#);
}

// ---------------------------------------------------------------------------
// apply with empty final list and only a list argument
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_empty_final_list_and_only_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Empty final list with spread args: all args come from spread
    assert_oracle_parity_with_bootstrap("(apply #'+ 1 2 3 '())");
    assert_oracle_parity_with_bootstrap("(apply #'list 'a 'b '())");
    // Only a list argument (no spread args)
    assert_oracle_parity_with_bootstrap("(apply #'+ '(100 200 300))");
    assert_oracle_parity_with_bootstrap("(apply #'list '(x y z))");
    // Empty final list, no spread args: zero-arg call
    assert_oracle_parity_with_bootstrap("(apply #'+ '())");
    assert_oracle_parity_with_bootstrap("(apply #'list '())");
    // Only nil as final list
    assert_oracle_parity_with_bootstrap("(apply #'+ nil)");
    // Deeply nested: apply constructing apply's args
    assert_oracle_parity_with_bootstrap("(apply #'+ (apply #'list 1 2 '(3 4)))");
    // apply with vector-producing function result as arg list
    // (mapcar produces a list, suitable as final arg)
    assert_oracle_parity_with_bootstrap("(apply #'+ (mapcar #'1+ '(0 1 2 3 4)))");
}

// ---------------------------------------------------------------------------
// funcall with lambda, closure, and subr targets
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_lambda_closure_subr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // funcall with a built-in subr
    assert_oracle_parity_with_bootstrap("(funcall #'+ 10 20 30)");
    assert_oracle_parity_with_bootstrap("(funcall #'concat \"a\" \"b\" \"c\")");
    assert_oracle_parity_with_bootstrap("(funcall #'car '(1 2 3))");

    // funcall with a lambda
    assert_oracle_parity_with_bootstrap("(funcall (lambda (x y z) (+ (* x y) z)) 3 4 5)");

    // funcall with a lexical closure (captures variable)
    assert_oracle_parity_with_bootstrap(
        r#"(let ((base 100))
             (let ((adder (lambda (x) (+ base x))))
               (list (funcall adder 1)
                     (funcall adder 50)
                     (funcall adder -100))))"#,
    );

    // funcall with a closure that captures a mutable cell
    assert_oracle_parity_with_bootstrap(
        r#"(let ((counter 0))
             (let ((inc (lambda () (setq counter (1+ counter)) counter))
                   (get (lambda () counter)))
               (list (funcall inc)
                     (funcall inc)
                     (funcall inc)
                     (funcall get))))"#,
    );

    // funcall with symbol naming a function
    let form = r#"(progn
      (fset 'neovm--test-afc-sq (lambda (x) (* x x)))
      (unwind-protect
          (list (funcall 'neovm--test-afc-sq 7)
                (funcall #'neovm--test-afc-sq 7)
                (funcall (symbol-function 'neovm--test-afc-sq) 7))
        (fmakunbound 'neovm--test-afc-sq)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// funcall with &rest parameters
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_rest_parameters() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple &rest: collect all args
    assert_oracle_parity_with_bootstrap("(funcall (lambda (&rest xs) xs) 1 2 3 4 5)");
    // &rest with no args
    assert_oracle_parity_with_bootstrap("(funcall (lambda (&rest xs) xs))");
    // Required arg + &rest
    assert_oracle_parity_with_bootstrap(
        "(funcall (lambda (head &rest tail) (cons head (length tail))) 'a 'b 'c 'd 'e)",
    );
    // apply with &rest function
    assert_oracle_parity_with_bootstrap(
        "(apply (lambda (a b &rest cs) (list a b cs)) 1 2 '(3 4 5))",
    );
    // Nested rest: inner function collects and outer spreads
    assert_oracle_parity_with_bootstrap(
        r#"(let ((collector (lambda (&rest items) (apply #'+ items))))
             (list (funcall collector 1 2 3)
                   (funcall collector)
                   (apply collector '(10 20 30))))"#,
    );
    // &rest with recursive processing
    assert_oracle_parity_with_bootstrap(
        r#"(let ((my-sum (lambda (&rest args)
                          (let ((total 0))
                            (dolist (x args total)
                              (setq total (+ total x)))))))
             (list (funcall my-sum 1 2 3 4 5)
                   (apply my-sum '(10 20 30))
                   (funcall my-sum)))"#,
    );
}

// ---------------------------------------------------------------------------
// funcall with &optional parameters
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_optional_parameters() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Single optional, supplied and not supplied
    assert_oracle_parity_with_bootstrap(
        "(list (funcall (lambda (a &optional b) (list a b)) 1)
               (funcall (lambda (a &optional b) (list a b)) 1 2))",
    );
    // Multiple optionals
    assert_oracle_parity_with_bootstrap(
        "(list (funcall (lambda (&optional a b c) (list a b c)))
               (funcall (lambda (&optional a b c) (list a b c)) 1)
               (funcall (lambda (&optional a b c) (list a b c)) 1 2)
               (funcall (lambda (&optional a b c) (list a b c)) 1 2 3))",
    );
    // &optional + &rest combined
    assert_oracle_parity_with_bootstrap(
        "(list (funcall (lambda (a &optional b &rest c) (list a b c)) 1)
               (funcall (lambda (a &optional b &rest c) (list a b c)) 1 2)
               (funcall (lambda (a &optional b &rest c) (list a b c)) 1 2 3 4 5))",
    );
    // Optional with default-like behavior via (or arg default)
    assert_oracle_parity_with_bootstrap(
        r#"(let ((make-greeter
                  (lambda (&optional name greeting)
                    (let ((n (or name "World"))
                          (g (or greeting "Hello")))
                      (concat g ", " n "!")))))
             (list (funcall make-greeter)
                   (funcall make-greeter "Alice")
                   (funcall make-greeter "Bob" "Hi")))"#,
    );
    // apply with optional params
    assert_oracle_parity_with_bootstrap("(apply (lambda (a &optional b c) (list a b c)) 1 '(2))");
}

// ---------------------------------------------------------------------------
// Nested apply/funcall chains
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nested_apply_funcall_chains() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // funcall returning function, called again
    assert_oracle_parity_with_bootstrap(
        "(funcall (funcall (lambda (x) (lambda (y) (* x y))) 6) 7)",
    );

    // Three levels of currying via nested funcall
    assert_oracle_parity_with_bootstrap(
        r#"(let ((curry3
                 (lambda (a)
                   (lambda (b)
                     (lambda (c)
                       (list a b c))))))
             (funcall (funcall (funcall curry3 'x) 'y) 'z))"#,
    );

    // apply inside funcall inside apply
    assert_oracle_parity_with_bootstrap(
        "(apply #'+ (funcall (lambda (xs) (mapcar #'1+ xs)) '(1 2 3)))",
    );

    // Chain: compose two functions, then apply the composition
    assert_oracle_parity_with_bootstrap(
        r#"(let ((compose
                 (lambda (f g)
                   (lambda (&rest args) (funcall f (apply g args))))))
             (let ((double-sum (funcall compose
                                        (lambda (x) (* x 2))
                                        #'+)))
               (list (funcall double-sum 1 2 3)
                     (funcall double-sum 10 20)
                     (apply double-sum '(5 5 5 5)))))"#,
    );

    // Mutual recursion via funcall with fset
    let form = r#"(progn
      (fset 'neovm--test-afc-even-p
        (lambda (n)
          (if (= n 0) t
            (funcall 'neovm--test-afc-odd-p (1- n)))))
      (fset 'neovm--test-afc-odd-p
        (lambda (n)
          (if (= n 0) nil
            (funcall 'neovm--test-afc-even-p (1- n)))))
      (unwind-protect
          (list (funcall 'neovm--test-afc-even-p 0)
                (funcall 'neovm--test-afc-even-p 1)
                (funcall 'neovm--test-afc-even-p 10)
                (funcall 'neovm--test-afc-odd-p 7)
                (funcall 'neovm--test-afc-odd-p 8))
        (fmakunbound 'neovm--test-afc-even-p)
        (fmakunbound 'neovm--test-afc-odd-p)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// apply/funcall with higher-order functions (mapcar result as arg)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_funcall_higher_order() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // apply #'append on mapcar result (flatten one level)
    assert_oracle_parity_with_bootstrap(
        "(apply #'append (mapcar (lambda (x) (list x (* x x))) '(1 2 3 4 5)))",
    );

    // funcall with result of mapcar as single arg
    assert_oracle_parity_with_bootstrap("(funcall #'length (mapcar #'1+ '(1 2 3 4 5 6 7 8 9 10)))");

    // Build a pipeline: list of functions, reduce with funcall
    assert_oracle_parity_with_bootstrap(
        r#"(let ((pipeline (list (lambda (x) (+ x 10))
                               (lambda (x) (* x 2))
                               (lambda (x) (- x 3)))))
             (let ((result 5))
               (dolist (fn pipeline result)
                 (setq result (funcall fn result)))))"#,
    );

    // apply with mapcar to transpose a matrix
    assert_oracle_parity_with_bootstrap("(apply #'mapcar #'list '((1 2 3) (4 5 6) (7 8 9)))");

    // Compose mapcar results with apply for zip-style operation
    assert_oracle_parity_with_bootstrap(
        r#"(let ((xs '(1 2 3 4))
                 (ys '(10 20 30 40)))
             (apply #'mapcar (lambda (a b) (+ a b)) (list xs ys)))"#,
    );

    // funcall a function selected from a dispatching alist
    assert_oracle_parity_with_bootstrap(
        r#"(let ((ops '((double . (lambda (x) (* x 2)))
                        (square . (lambda (x) (* x x)))
                        (negate . (lambda (x) (- x))))))
             (mapcar (lambda (pair)
                       (funcall (cdr (assq (car pair) ops)) (cdr pair)))
                     '((double . 5) (square . 4) (negate . 7) (double . 100))))"#,
    );
}

// ---------------------------------------------------------------------------
// Error cases: wrong number of args, non-function, apply with non-list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_funcall_error_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // funcall with too few args => error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (funcall (lambda (a b) (+ a b)) 1)
           (wrong-number-of-arguments
            (list 'wrong-number (cdr err))))"#,
    );

    // funcall with too many args => error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (funcall (lambda (a b) (+ a b)) 1 2 3)
           (wrong-number-of-arguments
            (list 'wrong-number (cdr err))))"#,
    );

    // funcall with non-function => error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (funcall 42 1 2)
           (invalid-function
            (list 'invalid-function (cadr err))))"#,
    );

    // apply with non-list as final arg => error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (apply #'+ 1 2 3)
           (wrong-type-argument
            (list 'wrong-type (car (cdr err)))))"#,
    );

    // funcall with void symbol => error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (funcall 'neovm--test-afc-nonexistent 1)
           (void-function
            (list 'void-function (cadr err))))"#,
    );

    // apply with &optional: too many args still works (extras ignored by &rest)
    // but without &rest, too many args is an error
    assert_oracle_parity_with_bootstrap(
        r#"(condition-case err
             (apply (lambda (a &optional b) (list a b)) '(1 2 3))
           (wrong-number-of-arguments
            (list 'wrong-number (cdr err))))"#,
    );
}

// ---------------------------------------------------------------------------
// Complex: apply/funcall with Y-combinator-like patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_funcall_y_combinator_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement factorial via a self-passing pattern (poor man's Y combinator)
    assert_oracle_parity_with_bootstrap(
        r#"(let ((fact-step
                 (lambda (self n)
                   (if (<= n 1) 1
                     (* n (funcall self self (1- n)))))))
             (list (funcall fact-step fact-step 0)
                   (funcall fact-step fact-step 1)
                   (funcall fact-step fact-step 5)
                   (funcall fact-step fact-step 10)))"#,
    );

    // Fibonacci via self-passing with memoization in a hash table
    assert_oracle_parity_with_bootstrap(
        r#"(let ((memo (make-hash-table :test 'eql)))
             (let ((fib-step
                    (lambda (self n)
                      (or (gethash n memo)
                          (let ((result
                                 (cond
                                  ((= n 0) 0)
                                  ((= n 1) 1)
                                  (t (+ (funcall self self (- n 1))
                                        (funcall self self (- n 2)))))))
                            (puthash n result memo)
                            result)))))
               (mapcar (lambda (k) (funcall fib-step fib-step k))
                       '(0 1 2 3 4 5 6 7 8 9 10 15 20))))"#,
    );

    // Apply with dynamically built argument lists
    assert_oracle_parity_with_bootstrap(
        r#"(let ((build-args
                 (lambda (n)
                   (let ((args nil) (i 0))
                     (while (< i n)
                       (setq args (cons (1+ i) args))
                       (setq i (1+ i)))
                     (nreverse args)))))
             (list (apply #'+ (funcall build-args 0))
                   (apply #'+ (funcall build-args 1))
                   (apply #'+ (funcall build-args 5))
                   (apply #'+ (funcall build-args 10))
                   (apply #'list (funcall build-args 4))))"#,
    );
}
