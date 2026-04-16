use super::*;
use crate::emacs_core::eval::Context;

#[test]
fn register_bootstrap_vars_matches_gnu_alloc_defaults() {
    crate::test_utils::init_test_tracing();
    let mut obarray = Obarray::new();
    register_bootstrap_vars(&mut obarray);

    assert_eq!(
        obarray.symbol_value("gc-cons-threshold").copied(),
        Some(Value::fixnum(800_000))
    );
    assert_eq!(
        obarray.symbol_value("garbage-collection-messages").copied(),
        Some(Value::NIL)
    );
    assert_eq!(
        obarray.symbol_value("post-gc-hook").copied(),
        Some(Value::NIL)
    );
    assert_eq!(
        obarray.symbol_value("memory-full").copied(),
        Some(Value::NIL)
    );
    assert_eq!(
        obarray.symbol_value("gcs-done").copied(),
        Some(Value::fixnum(0))
    );

    let signal_data = obarray
        .symbol_value("memory-signal-data")
        .copied()
        .expect("memory-signal-data");
    let items = list_to_vec(&signal_data).expect("memory-signal-data list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::symbol("error"));
    assert_eq!(
        items[1],
        Value::string("Memory exhausted--use M-x save-some-buffers then exit and restart Emacs")
    );
}

#[test]
fn evaluator_binds_alloc_bootstrap_vars() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    let obarray = eval.obarray();

    assert_eq!(
        obarray.symbol_value("memory-full").copied(),
        Some(Value::NIL)
    );
    assert_eq!(
        obarray.symbol_value("post-gc-hook").copied(),
        Some(Value::NIL)
    );

    let signal_data = obarray
        .symbol_value("memory-signal-data")
        .copied()
        .expect("memory-signal-data");
    let items = list_to_vec(&signal_data).expect("memory-signal-data list");
    assert_eq!(items[0], Value::symbol("error"));
}
