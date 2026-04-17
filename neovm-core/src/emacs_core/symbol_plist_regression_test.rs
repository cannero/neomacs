//! Regression tests for LispSymbol plist GNU-parity.
//!
//! Tests 1, 2, 6, 7 pass on both the pre-refactor HashMap storage and
//! the post-refactor cons-list storage. Tests 3, 4, 5 exercise
//! semantics only representable with a cons-list plist and are expected
//! to FAIL on HashMap storage and PASS after the field type flips.

use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;

fn make_ctx() -> Context {
    Context::new()
}

fn eval(ctx: &mut Context, src: &str) -> Value {
    ctx.eval_str(src).expect("eval")
}

fn print_val(value: &Value) -> String {
    crate::emacs_core::print::print_value(value)
}

#[test]
fn plist_put_get_round_trips() {
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(put 'plist-rt-foo 'color 'red)");
    eval(&mut ctx, "(put 'plist-rt-foo 'size 10)");
    assert_eq!(
        eval(&mut ctx, "(get 'plist-rt-foo 'color)"),
        Value::symbol("red")
    );
    assert_eq!(
        eval(&mut ctx, "(get 'plist-rt-foo 'size)"),
        Value::fixnum(10)
    );
}

#[test]
fn plist_get_missing_returns_nil() {
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(put 'plist-miss 'a 1)");
    assert_eq!(eval(&mut ctx, "(get 'plist-miss 'nope)"), Value::NIL);
}

#[test]
fn plist_insertion_order_preserved() {
    // GNU: (a 1 b 2 c 3). HashMap iteration order is arbitrary — fails today.
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(setplist 'plist-order nil)");
    eval(&mut ctx, "(put 'plist-order 'a 1)");
    eval(&mut ctx, "(put 'plist-order 'b 2)");
    eval(&mut ctx, "(put 'plist-order 'c 3)");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-order)");
    let printed = print_val(&plist);
    assert_eq!(printed, "(a 1 b 2 c 3)", "plist order drifted: {printed}");
}

#[test]
fn plist_duplicate_keys_preserved_by_setplist() {
    // GNU: (a 1 a 2). HashMap collapses to (a 2). Fails today.
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(setplist 'plist-dup '(a 1 a 2))");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-dup)");
    let printed = print_val(&plist);
    assert_eq!(printed, "(a 1 a 2)", "duplicate keys dropped: {printed}");
    assert_eq!(
        eval(&mut ctx, "(plist-get (symbol-plist 'plist-dup) 'a)"),
        Value::fixnum(1),
        "plist-get should return FIRST match"
    );
}

#[test]
fn symbol_plist_returns_eq_identical_pointer() {
    // GNU: two calls to (symbol-plist 'foo) return the SAME cons.
    // HashMap synthesizes a fresh list each call — (eq p1 p2) fails today.
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(put 'plist-eq 'a 1)");
    let first_eq = eval(
        &mut ctx,
        "(let ((p (symbol-plist 'plist-eq))) (eq p (symbol-plist 'plist-eq)))",
    );
    assert_eq!(first_eq, Value::T, "(eq p (symbol-plist foo)) must be t");
}

#[test]
fn setplist_accepts_and_preserves_arbitrary_list() {
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(setplist 'plist-setp '(x 10 y 20))");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-setp)");
    let printed = print_val(&plist);
    assert_eq!(printed, "(x 10 y 20)");
    assert_eq!(
        eval(&mut ctx, "(get 'plist-setp 'y)"),
        Value::fixnum(20)
    );
}

#[test]
fn plist_survives_gc() {
    crate::test_utils::init_test_tracing();
    let mut ctx = make_ctx();
    eval(&mut ctx, "(put 'plist-gc 'payload (cons 1 2))");
    let before = eval(&mut ctx, "(get 'plist-gc 'payload)");
    ctx.gc_collect();
    let after = eval(&mut ctx, "(get 'plist-gc 'payload)");
    assert!(
        crate::emacs_core::value::eq_value(&before.cons_car(), &Value::fixnum(1)),
        "car should be 1 before GC"
    );
    assert!(
        crate::emacs_core::value::eq_value(&after.cons_cdr(), &Value::fixnum(2)),
        "cdr should be 2 after GC — GC trace missed the plist value"
    );
}
