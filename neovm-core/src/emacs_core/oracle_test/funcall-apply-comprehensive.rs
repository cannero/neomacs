//! Comprehensive oracle parity tests for funcall/apply patterns:
//! funcall with 0-10+ args, apply with various arg list constructions,
//! funcall vs apply equivalence, funcall/apply with lambda/closures/subrs,
//! apply with improper arg lists, nested funcall/apply, funcall with &rest
//! functions, apply spreading behavior, higher-order function composition
//! via funcall chains, funcall-interactively, and macros (should error).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// funcall with 0 through 10+ arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_arg_count_sweep() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 0 args
    assert_oracle_parity("(funcall (lambda () 'zero-args))");
    // 1 arg
    assert_oracle_parity("(funcall (lambda (a) a) 'one)");
    // 2 args
    assert_oracle_parity("(funcall (lambda (a b) (list a b)) 1 2)");
    // 3 args
    assert_oracle_parity("(funcall (lambda (a b c) (+ a b c)) 10 20 30)");
    // 4 args
    assert_oracle_parity("(funcall (lambda (a b c d) (* (+ a b) (+ c d))) 1 2 3 4)");
    // 5 args
    assert_oracle_parity("(funcall (lambda (a b c d e) (list e d c b a)) 1 2 3 4 5)");
    // 6 args
    assert_oracle_parity("(funcall (lambda (a b c d e f) (+ a b c d e f)) 1 2 3 4 5 6)");
    // 7 args
    assert_oracle_parity(
        "(funcall (lambda (a b c d e f g) (list a (+ b c) (+ d e f g))) 1 2 3 4 5 6 7)",
    );
    // 8 args via built-in +
    assert_oracle_parity("(funcall #'+ 1 2 3 4 5 6 7 8)");
    // 9 args
    assert_oracle_parity("(funcall #'+ 1 2 3 4 5 6 7 8 9)");
    // 10 args
    assert_oracle_parity("(funcall #'+ 1 2 3 4 5 6 7 8 9 10)");
    // 12 args via list
    assert_oracle_parity("(funcall #'list 'a 'b 'c 'd 'e 'f 'g 'h 'i 'j 'k 'l)");
    // 15 args via concat
    assert_oracle_parity(
        r#"(funcall #'concat "a" "b" "c" "d" "e" "f" "g" "h" "i" "j" "k" "l" "m" "n" "o")"#,
    );
}

// ---------------------------------------------------------------------------
// funcall vs apply equivalence for various arg patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_apply_equivalence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // funcall with explicit args should equal apply with those args as a list
    assert_oracle_parity(
        r#"(let ((f (lambda (a b c) (+ a b c))))
             (list (= (funcall f 10 20 30)
                       (apply f '(10 20 30)))
                   (= (funcall f 1 2 3)
                       (apply f 1 '(2 3)))
                   (= (funcall f 100 200 300)
                       (apply f 100 200 '(300)))))"#,
    );

    // Equivalence with &rest
    assert_oracle_parity(
        r#"(let ((f (lambda (&rest xs) (apply #'+ xs))))
             (list (equal (funcall f) (apply f nil))
                   (equal (funcall f 1) (apply f '(1)))
                   (equal (funcall f 1 2 3) (apply f '(1 2 3)))
                   (equal (funcall f 1 2 3 4 5) (apply f 1 2 3 '(4 5)))))"#,
    );

    // Equivalence with &optional
    assert_oracle_parity(
        r#"(let ((f (lambda (a &optional b c) (list a b c))))
             (list (equal (funcall f 1) (apply f '(1)))
                   (equal (funcall f 1 2) (apply f 1 '(2)))
                   (equal (funcall f 1 2 3) (apply f '(1 2 3)))))"#,
    );

    // Equivalence with subrs
    assert_oracle_parity(
        r#"(list (equal (funcall #'list 1 2 3) (apply #'list '(1 2 3)))
               (equal (funcall #'+ 10 20) (apply #'+ '(10 20)))
               (equal (funcall #'cons 'a 'b) (apply #'cons '(a b)))
               (equal (funcall #'concat "x" "y") (apply #'concat '("x" "y"))))"#,
    );
}

// ---------------------------------------------------------------------------
// apply spreading behavior: multiple spread args before final list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_spreading_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 0 spread args + full list
    assert_oracle_parity("(apply #'+ '(1 2 3 4 5))");
    // 1 spread + list
    assert_oracle_parity("(apply #'+ 100 '(1 2))");
    // 2 spread + list
    assert_oracle_parity("(apply #'list 'a 'b '(c d))");
    // 3 spread + list
    assert_oracle_parity("(apply #'+ 1 2 3 '(4 5 6))");
    // 4 spread + list
    assert_oracle_parity("(apply #'list 'a 'b 'c 'd '(e f))");
    // 5 spread + empty list (all from spread)
    assert_oracle_parity("(apply #'+ 1 2 3 4 5 '())");
    // 6 spread + nil
    assert_oracle_parity("(apply #'list 1 2 3 4 5 6 nil)");
    // Spread args are complex expressions
    assert_oracle_parity("(apply #'+ (* 2 3) (+ 4 5) (- 10 3) '((+ 1 1)))");
    // Apply with cons-constructed final arg
    assert_oracle_parity("(apply #'list 'head (cons 'a (cons 'b nil)))");
    // Apply with append-constructed final arg
    assert_oracle_parity("(apply #'+ (append '(1 2) '(3 4)))");
    // Apply with mapcar-constructed final arg
    assert_oracle_parity("(apply #'+ (mapcar #'1+ '(0 1 2 3 4)))");
}

// ---------------------------------------------------------------------------
// apply with empty and nil arg lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_empty_and_nil_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // apply + with empty list => 0
    assert_oracle_parity("(apply #'+ '())");
    // apply + with nil => 0
    assert_oracle_parity("(apply #'+ nil)");
    // apply list with nil => ()
    assert_oracle_parity("(apply #'list nil)");
    // apply list with empty list => ()
    assert_oracle_parity("(apply #'list '())");
    // apply concat with nil => ""
    assert_oracle_parity("(apply #'concat nil)");
    // apply with spread args and empty final list
    assert_oracle_parity("(apply #'list 'a 'b 'c '())");
    // apply with only a single-element list
    assert_oracle_parity("(apply #'1+ '(41))");
    // apply with deeply nested argument construction
    assert_oracle_parity(
        "(apply #'+ (let ((r nil)) (dotimes (i 10) (setq r (cons (1+ i) r))) (nreverse r)))",
    );
}

// ---------------------------------------------------------------------------
// funcall/apply with closures capturing mutable state
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_apply_closures_mutable_state() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Counter closure called via funcall and apply
    assert_oracle_parity(
        r#"(let ((n 0))
             (let ((inc (lambda (&optional amount)
                          (setq n (+ n (or amount 1)))
                          n)))
               (list (funcall inc)
                     (funcall inc)
                     (funcall inc 5)
                     (apply inc '(10))
                     (apply inc nil)
                     n)))"#,
    );

    // Closure over a list, mutated via nconc
    assert_oracle_parity(
        r#"(let ((log nil))
             (let ((logger (lambda (&rest msgs)
                             (setq log (append log msgs))
                             (length log))))
               (list (funcall logger 'a)
                     (funcall logger 'b 'c)
                     (apply logger '(d e f))
                     (funcall logger)
                     log)))"#,
    );

    // Closure factory: each call creates a new closure sharing state
    assert_oracle_parity(
        r#"(let ((state 0))
             (let ((make-adder (lambda (base)
                                 (lambda (x)
                                   (setq state (+ state 1))
                                   (+ base x state)))))
               (let ((add10 (funcall make-adder 10))
                     (add20 (funcall make-adder 20)))
                 (list (funcall add10 1)
                       (funcall add20 2)
                       (funcall add10 3)
                       (apply add20 '(4))
                       state))))"#,
    );
}

// ---------------------------------------------------------------------------
// funcall/apply with subrs: all major built-in types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_apply_subr_variety() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Arithmetic subrs
    assert_oracle_parity("(funcall #'+ 1 2 3)");
    assert_oracle_parity("(funcall #'- 10 3 2)");
    assert_oracle_parity("(funcall #'* 2 3 4)");
    assert_oracle_parity("(funcall #'/ 100 5 4)");
    assert_oracle_parity("(funcall #'% 17 5)");
    assert_oracle_parity("(funcall #'mod 17 5)");

    // Comparison subrs
    assert_oracle_parity("(funcall #'< 1 2 3)");
    assert_oracle_parity("(funcall #'> 3 2 1)");
    assert_oracle_parity("(funcall #'= 5 5 5)");
    assert_oracle_parity("(funcall #'<= 1 1 2)");
    assert_oracle_parity("(funcall #'>= 3 3 2)");

    // String subrs
    assert_oracle_parity(r#"(funcall #'concat "hello" " " "world")"#);
    assert_oracle_parity(r#"(funcall #'string-to-number "42")"#);
    assert_oracle_parity(r#"(funcall #'substring "hello world" 6)"#);
    assert_oracle_parity(r#"(funcall #'upcase "abc")"#);

    // List subrs
    assert_oracle_parity("(funcall #'cons 'a '(b c))");
    assert_oracle_parity("(funcall #'car '(1 2 3))");
    assert_oracle_parity("(funcall #'cdr '(1 2 3))");
    assert_oracle_parity("(funcall #'length '(a b c d))");
    assert_oracle_parity("(funcall #'nth 2 '(a b c d))");
    assert_oracle_parity("(funcall #'nthcdr 2 '(a b c d))");
    assert_oracle_parity("(funcall #'reverse '(1 2 3 4 5))");
    assert_oracle_parity("(funcall #'append '(1 2) '(3 4) '(5))");

    // Predicate subrs
    assert_oracle_parity("(funcall #'null nil)");
    assert_oracle_parity("(funcall #'null t)");
    assert_oracle_parity("(funcall #'numberp 42)");
    assert_oracle_parity("(funcall #'stringp \"hi\")");
    assert_oracle_parity("(funcall #'symbolp 'foo)");
    assert_oracle_parity("(funcall #'consp '(1))");
    assert_oracle_parity("(funcall #'listp '(1 2))");
}

// ---------------------------------------------------------------------------
// apply with non-list final arg should error
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_non_list_final_arg_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // apply with integer as final arg
    assert_oracle_parity(
        r#"(condition-case err
             (apply #'+ 1 2 3)
           (wrong-type-argument (list 'got-error (car err))))"#,
    );
    // apply with string as final arg
    assert_oracle_parity(
        r#"(condition-case err
             (apply #'+ "not-a-list")
           (wrong-type-argument (list 'got-error (car err))))"#,
    );
    // apply with symbol as final arg (not nil)
    assert_oracle_parity(
        r#"(condition-case err
             (apply #'+ 'not-a-list)
           (wrong-type-argument (list 'got-error (car err))))"#,
    );
    // apply with vector as final arg
    assert_oracle_parity(
        r#"(condition-case err
             (apply #'+ [1 2 3])
           (wrong-type-argument (list 'got-error (car err))))"#,
    );
}

// ---------------------------------------------------------------------------
// funcall with macros should error (invalid-function)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_macro_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Calling a macro via funcall should signal invalid-function or similar
    let form = r#"(progn
      (defmacro neovm--test-fac-mac (x) (list '+ x 1))
      (unwind-protect
          (condition-case err
              (funcall (symbol-function 'neovm--test-fac-mac) 5)
            (invalid-function (list 'invalid-function-caught))
            (error (list 'other-error (car err))))
        (fmakunbound 'neovm--test-fac-mac)))"#;
    assert_oracle_parity(form);

    // apply with macro should also error
    let form2 = r#"(progn
      (defmacro neovm--test-fac-mac2 (x) (list '* x 2))
      (unwind-protect
          (condition-case err
              (apply (symbol-function 'neovm--test-fac-mac2) '(5))
            (invalid-function (list 'invalid-function-caught))
            (error (list 'other-error (car err))))
        (fmakunbound 'neovm--test-fac-mac2)))"#;
    assert_oracle_parity(form2);
}

// ---------------------------------------------------------------------------
// Nested funcall/apply chains: currying, composition, pipelines
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_nested_funcall_apply_deep() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 4-level currying
    assert_oracle_parity(
        r#"(let ((f (lambda (a)
                     (lambda (b)
                       (lambda (c)
                         (lambda (d) (list a b c d)))))))
             (funcall (funcall (funcall (funcall f 1) 2) 3) 4))"#,
    );

    // funcall result as arg to apply
    assert_oracle_parity(r#"(apply #'+ (funcall #'list 1 2 3 4 5))"#);

    // apply result as arg to funcall
    assert_oracle_parity(r#"(funcall #'1+ (apply #'+ '(1 2 3 4)))"#);

    // Double composition
    assert_oracle_parity(
        r#"(let ((compose (lambda (f g) (lambda (&rest args) (funcall f (apply g args))))))
             (let ((add1-then-double (funcall compose (lambda (x) (* x 2)) #'+))
                   (double-then-add1 (funcall compose #'1+ (lambda (&rest xs) (* 2 (apply #'+ xs))))))
               (list (funcall add1-then-double 3 4)
                     (funcall double-then-add1 3 4))))"#,
    );

    // Pipeline of 5 functions
    assert_oracle_parity(
        r#"(let ((pipe (lambda (fns val)
                         (let ((result val))
                           (dolist (f fns result)
                             (setq result (funcall f result)))))))
             (funcall pipe
                      (list #'1+ (lambda (x) (* x 3)) #'1+ (lambda (x) (- x 5)) #'abs)
                      10))"#,
    );

    // Mutual recursion via funcall depth=20
    let form = r#"(progn
      (fset 'neovm--test-fac-is-even
        (lambda (n) (if (= n 0) t (funcall 'neovm--test-fac-is-odd (1- n)))))
      (fset 'neovm--test-fac-is-odd
        (lambda (n) (if (= n 0) nil (funcall 'neovm--test-fac-is-even (1- n)))))
      (unwind-protect
          (list (funcall 'neovm--test-fac-is-even 0)
                (funcall 'neovm--test-fac-is-even 1)
                (funcall 'neovm--test-fac-is-even 10)
                (funcall 'neovm--test-fac-is-even 19)
                (funcall 'neovm--test-fac-is-odd 0)
                (funcall 'neovm--test-fac-is-odd 1)
                (funcall 'neovm--test-fac-is-odd 20)
                (funcall 'neovm--test-fac-is-odd 21))
        (fmakunbound 'neovm--test-fac-is-even)
        (fmakunbound 'neovm--test-fac-is-odd)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// funcall with &rest: various combinations of fixed + rest args
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_rest_arg_combinations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Pure &rest, 0 args
    assert_oracle_parity("(funcall (lambda (&rest xs) (length xs)))");
    // Pure &rest, 1 arg
    assert_oracle_parity("(funcall (lambda (&rest xs) xs) 42)");
    // Pure &rest, many args
    assert_oracle_parity("(funcall (lambda (&rest xs) xs) 1 2 3 4 5 6 7 8)");

    // 1 required + &rest, 0 rest
    assert_oracle_parity("(funcall (lambda (a &rest xs) (list a xs)) 'only)");
    // 1 required + &rest, many rest
    assert_oracle_parity(
        "(funcall (lambda (a &rest xs) (list a (length xs) (apply #'+ xs))) 'h 1 2 3 4 5)",
    );

    // 2 required + &rest
    assert_oracle_parity("(funcall (lambda (a b &rest xs) (list a b xs)) 1 2)");
    assert_oracle_parity("(funcall (lambda (a b &rest xs) (list a b xs)) 1 2 3 4 5)");

    // 1 required + 1 optional + &rest
    assert_oracle_parity("(funcall (lambda (a &optional b &rest xs) (list a b xs)) 1)");
    assert_oracle_parity("(funcall (lambda (a &optional b &rest xs) (list a b xs)) 1 2)");
    assert_oracle_parity("(funcall (lambda (a &optional b &rest xs) (list a b xs)) 1 2 3 4)");

    // 2 optional + &rest
    assert_oracle_parity("(funcall (lambda (&optional a b &rest xs) (list a b xs)))");
    assert_oracle_parity("(funcall (lambda (&optional a b &rest xs) (list a b xs)) 'x)");
    assert_oracle_parity("(funcall (lambda (&optional a b &rest xs) (list a b xs)) 'x 'y 'z1 'z2)");
}

// ---------------------------------------------------------------------------
// apply spreading with &rest functions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_spreading_with_rest() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // apply + rest: spread args contribute to both fixed and rest
    assert_oracle_parity("(apply (lambda (a &rest xs) (cons a xs)) 'first '(second third))");
    assert_oracle_parity("(apply (lambda (a b &rest xs) (list a b xs)) 1 2 '(3 4 5))");
    assert_oracle_parity("(apply (lambda (a b &rest xs) (list a b xs)) 1 '(2))");

    // apply where all args come from the spread list
    assert_oracle_parity("(apply (lambda (a b c &rest xs) (list a b c xs)) '(10 20 30 40 50))");

    // apply with dynamically constructed arg list
    assert_oracle_parity(
        r#"(let ((args (number-sequence 1 10)))
             (apply (lambda (&rest xs) (apply #'+ xs)) args))"#,
    );
}

// ---------------------------------------------------------------------------
// Higher-order function composition via funcall chains
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_higher_order_composition() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // map + filter + reduce via funcall
    assert_oracle_parity(
        r#"(let ((my-filter (lambda (pred lst)
                              (let ((result nil))
                                (dolist (x lst (nreverse result))
                                  (when (funcall pred x)
                                    (setq result (cons x result)))))))
               (my-reduce (lambda (fn init lst)
                            (let ((acc init))
                              (dolist (x lst acc)
                                (setq acc (funcall fn acc x)))))))
             (let* ((data '(1 2 3 4 5 6 7 8 9 10))
                    (evens (funcall my-filter #'evenp data))
                    (odds (funcall my-filter #'oddp data))
                    (sum-evens (funcall my-reduce #'+ 0 evens))
                    (prod-odds (funcall my-reduce #'* 1 odds)))
               (list evens odds sum-evens prod-odds)))"#,
    );

    // Partial application helper
    assert_oracle_parity(
        r#"(let ((partial (lambda (f &rest initial-args)
                            (lambda (&rest more-args)
                              (apply f (append initial-args more-args))))))
             (let ((add5 (funcall partial #'+ 5))
                   (mul3 (funcall partial #'* 3))
                   (prefix-list (funcall partial #'list 'header)))
               (list (funcall add5 10)
                     (funcall add5 0)
                     (funcall mul3 7)
                     (funcall prefix-list 'a 'b 'c))))"#,
    );

    // Function dispatch table using alist + funcall
    assert_oracle_parity(
        r#"(let ((dispatch '((add . +) (sub . -) (mul . *) (div . /))))
             (let ((run-op (lambda (op &rest args)
                             (let ((fn (cdr (assq op dispatch))))
                               (if fn (apply fn args) (error "Unknown op"))))))
               (list (funcall run-op 'add 1 2 3)
                     (funcall run-op 'sub 10 3)
                     (funcall run-op 'mul 2 3 4)
                     (funcall run-op 'div 100 5 4))))"#,
    );
}

// ---------------------------------------------------------------------------
// funcall with symbol-function indirection
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_symbol_function_indirection() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Three ways to call via symbol: quote, sharp-quote, symbol-function
    let form = r#"(progn
      (fset 'neovm--test-fac-triple (lambda (x) (* x 3)))
      (unwind-protect
          (list
           ;; Via quoted symbol
           (funcall 'neovm--test-fac-triple 7)
           ;; Via sharp-quote
           (funcall #'neovm--test-fac-triple 7)
           ;; Via symbol-function
           (funcall (symbol-function 'neovm--test-fac-triple) 7)
           ;; Apply all three ways
           (apply 'neovm--test-fac-triple '(10))
           (apply #'neovm--test-fac-triple '(10))
           (apply (symbol-function 'neovm--test-fac-triple) '(10))
           ;; Verify they all produce the same result
           (= (funcall 'neovm--test-fac-triple 5)
              (funcall #'neovm--test-fac-triple 5)
              (funcall (symbol-function 'neovm--test-fac-triple) 5)))
        (fmakunbound 'neovm--test-fac-triple)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Wrong number of arguments errors
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_wrong_arg_count_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Too few args to fixed-arity lambda
    assert_oracle_parity(
        r#"(condition-case err
             (funcall (lambda (a b c) (+ a b c)) 1 2)
           (wrong-number-of-arguments 'too-few))"#,
    );

    // Too many args to fixed-arity lambda
    assert_oracle_parity(
        r#"(condition-case err
             (funcall (lambda (a b) (+ a b)) 1 2 3)
           (wrong-number-of-arguments 'too-many))"#,
    );

    // Too few for required + optional (need at least 1)
    assert_oracle_parity(
        r#"(condition-case err
             (funcall (lambda (a &optional b) (list a b)))
           (wrong-number-of-arguments 'too-few))"#,
    );

    // Too many for required + optional (no &rest)
    assert_oracle_parity(
        r#"(condition-case err
             (funcall (lambda (a &optional b) (list a b)) 1 2 3)
           (wrong-number-of-arguments 'too-many))"#,
    );

    // funcall with non-function value
    assert_oracle_parity(
        r#"(condition-case err
             (funcall 42)
           (invalid-function (list 'invalid-function (cadr err))))"#,
    );

    // funcall with void symbol
    assert_oracle_parity(
        r#"(condition-case err
             (funcall 'neovm--test-fac-nonexistent-fn123)
           (void-function (list 'void (cadr err))))"#,
    );

    // apply with too few for required params
    assert_oracle_parity(
        r#"(condition-case err
             (apply (lambda (a b c) (+ a b c)) '(1))
           (wrong-number-of-arguments 'too-few-apply))"#,
    );
}

// ---------------------------------------------------------------------------
// Complex: Y-combinator style self-application and trampolining
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_apply_self_application() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Factorial via self-passing
    assert_oracle_parity(
        r#"(let ((fact (lambda (self n)
                         (if (<= n 1) 1
                           (* n (funcall self self (1- n)))))))
             (list (funcall fact fact 0)
                   (funcall fact fact 1)
                   (funcall fact fact 5)
                   (funcall fact fact 10)
                   (funcall fact fact 12)))"#,
    );

    // Fibonacci via self-passing with memoization
    assert_oracle_parity(
        r#"(let ((memo (make-hash-table :test 'eql)))
             (let ((fib (lambda (self n)
                          (or (gethash n memo)
                              (let ((r (if (< n 2) n
                                         (+ (funcall self self (- n 1))
                                            (funcall self self (- n 2))))))
                                (puthash n r memo)
                                r)))))
               (mapcar (lambda (k) (funcall fib fib k))
                       '(0 1 2 3 4 5 10 15 20 25))))"#,
    );

    // Trampoline pattern: functions return thunks until non-function
    assert_oracle_parity(
        r#"(let ((trampoline (lambda (fn &rest args)
                                (let ((result (apply fn args)))
                                  (while (functionp result)
                                    (setq result (funcall result)))
                                  result))))
             (let ((count-down (lambda (n acc)
                                 (if (= n 0) acc
                                   (let ((nn (1- n)) (aa (1+ acc)))
                                     (lambda () (funcall 'neovm--test-fac-cd nn aa)))))))
               (fset 'neovm--test-fac-cd count-down)
               (unwind-protect
                   (list (funcall trampoline count-down 0 0)
                         (funcall trampoline count-down 5 0)
                         (funcall trampoline count-down 100 0))
                 (fmakunbound 'neovm--test-fac-cd))))"#,
    );
}

// ---------------------------------------------------------------------------
// funcall/apply with hash-table and vector manipulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_apply_data_structure_ops() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use funcall to build and query hash tables
    assert_oracle_parity(
        r#"(let ((ht (make-hash-table :test 'equal)))
             (let ((set-val (lambda (k v) (puthash k v ht)))
                   (get-val (lambda (k) (gethash k ht 'missing))))
               (funcall set-val "name" "Alice")
               (funcall set-val "age" 30)
               (funcall set-val "score" 95)
               (list (funcall get-val "name")
                     (funcall get-val "age")
                     (funcall get-val "score")
                     (funcall get-val "nonexistent")
                     (hash-table-count ht))))"#,
    );

    // Use apply with vector operations
    assert_oracle_parity(
        r#"(let ((v [10 20 30 40 50]))
             (list (apply #'+ (append v nil))
                   (funcall #'aref v 0)
                   (funcall #'aref v 4)
                   (funcall #'length v)))"#,
    );

    // Reduce over a vector via funcall
    assert_oracle_parity(
        r#"(let ((vec [3 1 4 1 5 9 2 6]))
             (let ((max-val (aref vec 0))
                   (min-val (aref vec 0))
                   (sum 0))
               (dotimes (i (length vec))
                 (let ((v (aref vec i)))
                   (setq max-val (funcall #'max max-val v))
                   (setq min-val (funcall #'min min-val v))
                   (setq sum (funcall #'+ sum v))))
               (list max-val min-val sum)))"#,
    );
}
