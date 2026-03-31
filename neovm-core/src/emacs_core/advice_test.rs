use super::super::expr::Expr;
use super::super::intern::intern;
use super::super::value::{LambdaData, LambdaParams};
use super::*;

// -----------------------------------------------------------------------
// VariableWatcherList tests
// -----------------------------------------------------------------------

#[test]
fn add_and_notify_watcher() {
    let mut wl = VariableWatcherList::new();
    assert!(!wl.has_watchers("my-var"));

    wl.add_watcher("my-var", Value::symbol("my-watcher"));
    assert!(wl.has_watchers("my-var"));

    let calls = wl.notify_watchers(
        "my-var",
        &Value::fixnum(42),
        &Value::fixnum(0),
        "set",
        &Value::NIL,
    );
    assert_eq!(calls.len(), 1);

    let (callback, args) = &calls[0];
    assert!(callback.is_symbol_named("my-watcher"));
    assert_eq!(args.len(), 4);
    // arg 0: symbol name
    assert!(args[0].is_symbol_named("my-var"));
    // arg 1: new value
    assert!(matches!(&args[1], Value::fixnum(42)));
    // arg 2: operation
    assert!(args[2].is_symbol_named("set"));
    // arg 3: where (nil)
    assert!(matches!(&args[3], Value::NIL));
}

#[test]
fn remove_watcher() {
    let mut wl = VariableWatcherList::new();
    wl.add_watcher("my-var", Value::symbol("watcher1"));
    wl.add_watcher("my-var", Value::symbol("watcher2"));
    assert!(wl.has_watchers("my-var"));

    wl.remove_watcher("my-var", &Value::symbol("watcher1"));
    let calls = wl.notify_watchers("my-var", &Value::fixnum(1), &Value::fixnum(0), "set", &Value::NIL);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.is_symbol_named("watcher2"));
}

#[test]
fn remove_all_watchers_cleans_up() {
    let mut wl = VariableWatcherList::new();
    wl.add_watcher("my-var", Value::symbol("w1"));

    wl.remove_watcher("my-var", &Value::symbol("w1"));
    assert!(!wl.has_watchers("my-var"));
}

#[test]
fn no_duplicate_watchers() {
    let mut wl = VariableWatcherList::new();
    wl.add_watcher("my-var", Value::symbol("w"));
    wl.add_watcher("my-var", Value::symbol("w"));

    let calls = wl.notify_watchers("my-var", &Value::fixnum(1), &Value::fixnum(0), "set", &Value::NIL);
    assert_eq!(calls.len(), 1);
}

#[test]
fn no_duplicate_equivalent_lambda_watchers() {
    let mut wl = VariableWatcherList::new();
    let callback_a = Value::make_lambda(LambdaData {
        params: LambdaParams {
            required: vec![
                intern("symbol"),
                intern("newval"),
                intern("operation"),
                intern("where"),
            ],
            optional: Vec::new(),
            rest: None,
        },
        body: vec![Expr::Int(0)].into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    let callback_b = Value::make_lambda(LambdaData {
        params: LambdaParams {
            required: vec![
                intern("symbol"),
                intern("newval"),
                intern("operation"),
                intern("where"),
            ],
            optional: Vec::new(),
            rest: None,
        },
        body: vec![Expr::Int(0)].into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });

    wl.add_watcher("my-var", callback_a);
    wl.add_watcher("my-var", callback_b);
    assert_eq!(wl.get_watchers("my-var"), vec![callback_a]);
}

#[test]
fn notify_no_watchers_returns_empty() {
    let wl = VariableWatcherList::new();
    let calls = wl.notify_watchers("no-var", &Value::fixnum(1), &Value::fixnum(0), "set", &Value::NIL);
    assert!(calls.is_empty());
}

#[test]
fn multiple_watchers_all_notified() {
    let mut wl = VariableWatcherList::new();
    wl.add_watcher("v", Value::symbol("w1"));
    wl.add_watcher("v", Value::symbol("w2"));
    wl.add_watcher("v", Value::symbol("w3"));

    let calls = wl.notify_watchers("v", &Value::fixnum(99), &Value::fixnum(0), "set", &Value::NIL);
    assert_eq!(calls.len(), 3);
}

#[test]
fn get_watchers_returns_callbacks_in_registration_order() {
    let mut wl = VariableWatcherList::new();
    wl.add_watcher("v", Value::symbol("w1"));
    wl.add_watcher("v", Value::symbol("w2"));

    let watchers = wl.get_watchers("v");
    assert_eq!(watchers, vec![Value::symbol("w1"), Value::symbol("w2")]);
    assert!(wl.get_watchers("missing").is_empty());
}

#[test]
fn builtin_get_variable_watchers_tracks_runtime_registry() {
    let mut eval = super::super::eval::Context::new();
    builtin_add_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watched-var"), Value::symbol("watch-a")],
    )
    .unwrap();
    builtin_add_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watched-var"), Value::symbol("watch-b")],
    )
    .unwrap();

    let watchers =
        builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watched-var")]).unwrap();
    let watchers_vec = super::super::value::list_to_vec(&watchers).expect("watcher list");
    assert_eq!(
        watchers_vec,
        vec![Value::symbol("watch-a"), Value::symbol("watch-b")]
    );

    builtin_remove_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watched-var"), Value::symbol("watch-a")],
    )
    .unwrap();
    let remaining =
        builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watched-var")]).unwrap();
    assert_eq!(
        super::super::value::list_to_vec(&remaining).expect("watcher list"),
        vec![Value::symbol("watch-b")]
    );

    let wrong_type = builtin_get_variable_watchers(&mut eval, vec![Value::fixnum(1)]).unwrap_err();
    match wrong_type {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn variable_watcher_builtins_follow_runtime_alias_resolution() {
    let mut eval = super::super::eval::Context::new();
    super::super::builtins::builtin_defvaralias(
        &mut eval,
        vec![
            Value::symbol("vm-watch-alias"),
            Value::symbol("vm-watch-base"),
        ],
    )
    .expect("defvaralias should install alias edge");

    builtin_add_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watch-alias"), Value::symbol("watch-a")],
    )
    .expect("add-variable-watcher should resolve alias");

    let via_alias = builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-alias")])
        .expect("get-variable-watchers should resolve alias");
    assert_eq!(
        super::super::value::list_to_vec(&via_alias).expect("watcher list"),
        vec![Value::symbol("watch-a")]
    );

    let via_base = builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-base")])
        .expect("get-variable-watchers should resolve base");
    assert_eq!(
        super::super::value::list_to_vec(&via_base).expect("watcher list"),
        vec![Value::symbol("watch-a")]
    );

    builtin_remove_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watch-alias"), Value::symbol("watch-a")],
    )
    .expect("remove-variable-watcher should resolve alias");
    let remaining = builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-base")])
        .expect("get-variable-watchers should return empty after removal");
    assert!(remaining.is_nil());
}

#[test]
fn remove_variable_watcher_accepts_non_symbol_callbacks() {
    let mut eval = super::super::eval::Context::new();
    let callback = Value::make_lambda(LambdaData {
        params: LambdaParams {
            required: vec![
                intern("symbol"),
                intern("newval"),
                intern("operation"),
                intern("where"),
            ],
            optional: Vec::new(),
            rest: None,
        },
        body: vec![Expr::Symbol(intern("newval"))].into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    let equivalent_callback = Value::make_lambda(LambdaData {
        params: LambdaParams {
            required: vec![
                intern("symbol"),
                intern("newval"),
                intern("operation"),
                intern("where"),
            ],
            optional: Vec::new(),
            rest: None,
        },
        body: vec![Expr::Symbol(intern("newval"))].into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });

    builtin_add_variable_watcher(&mut eval, vec![Value::symbol("vm-watch-nonsym"), callback])
        .expect("add-variable-watcher should accept lambda callbacks");
    let before = builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-nonsym")])
        .expect("get-variable-watchers should return lambda callback");
    assert_eq!(
        super::super::value::list_to_vec(&before).expect("watcher list"),
        vec![callback]
    );

    builtin_remove_variable_watcher(
        &mut eval,
        vec![Value::symbol("vm-watch-nonsym"), equivalent_callback],
    )
    .expect("remove-variable-watcher should remove equivalent lambda callbacks");
    let after = builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-nonsym")])
        .expect("get-variable-watchers should be empty after removal");
    assert!(after.is_nil());
}
