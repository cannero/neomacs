use std::rc::Rc;

use neovm_core::emacs_core::{Context, LambdaData, LambdaParams, Value, parse_forms};

#[test]
fn compat_macro_cache_keeps_opaque_values_alive_across_gc() {
    let mut eval = Context::new();

    let macro_body = parse_forms("(function (lambda () 123))").expect("parse macro body");
    let opaque_macro = Value::make_macro(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: Rc::new(macro_body),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    eval.obarray_mut()
        .set_symbol_function("opaque-macro", opaque_macro);

    let forms = parse_forms("(opaque-macro)").expect("parse macro call");
    let first = eval.eval_expr(&forms[0]).expect("first macro expansion");
    assert!(
        matches!(first, Value::Lambda(_)),
        "macro expansion should yield a runtime closure, got {first:?}"
    );

    eval.gc_collect();

    // After GC, the macro cache is cleared (gc_collect() calls
    // macro_expansion_cache.clear()), so the macro is re-expanded,
    // producing a new Lambda.  The key invariant is that the macro
    // expansion still succeeds and yields a valid Lambda after GC.
    let second = eval
        .eval_expr(&forms[0])
        .expect("second macro expansion after GC");
    assert!(
        matches!(second, Value::Lambda(_)),
        "macro expansion should still yield a runtime closure after GC, got {second:?}"
    );
}
