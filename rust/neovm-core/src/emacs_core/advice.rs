//! Variable watchers for the Elisp VM.
//!
//! Provides callbacks invoked when a watched variable changes
//! (like Emacs `add-variable-watcher` / `remove-variable-watcher`).

use std::collections::HashMap;

use super::intern::resolve_sym;
use super::value::Value;
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Variable watcher system
// ---------------------------------------------------------------------------

/// A single variable watcher callback.
#[derive(Clone, Debug)]
pub struct VariableWatcher {
    /// The callback function to invoke on variable change.
    pub callback: Value,
}

/// Registry of variable watchers.
pub struct VariableWatcherList {
    /// Map from variable name → list of watcher callbacks.
    watchers: HashMap<String, Vec<VariableWatcher>>,
}

impl VariableWatcherList {
    pub fn new() -> Self {
        Self {
            watchers: HashMap::new(),
        }
    }

    /// Add a watcher callback for a variable.
    pub fn add_watcher(&mut self, var_name: &str, callback: Value) {
        let entry = self.watchers.entry(var_name.to_string()).or_default();
        // Don't add duplicate watchers.
        let already_exists = entry
            .iter()
            .any(|w| watcher_callback_matches(&w.callback, &callback));
        if !already_exists {
            entry.push(VariableWatcher { callback });
        }
    }

    /// Remove a watcher callback for a variable.
    pub fn remove_watcher(&mut self, var_name: &str, callback: &Value) {
        if let Some(list) = self.watchers.get_mut(var_name) {
            list.retain(|w| !watcher_callback_matches(&w.callback, callback));
            if list.is_empty() {
                self.watchers.remove(var_name);
            }
        }
    }

    /// Remove all watcher callbacks for a variable.
    pub fn clear_watchers(&mut self, var_name: &str) {
        self.watchers.remove(var_name);
    }

    /// Check if a variable has any watchers.
    pub fn has_watchers(&self, var_name: &str) -> bool {
        self.watchers
            .get(var_name)
            .is_some_and(|list| !list.is_empty())
    }

    /// Return registered watcher callbacks for a variable in insertion order.
    pub fn get_watchers(&self, var_name: &str) -> Vec<Value> {
        self.watchers
            .get(var_name)
            .map(|list| {
                list.iter()
                    .map(|watcher| watcher.callback)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Build a list of (callback, args) pairs to invoke for a variable change.
    ///
    /// Returns a Vec of (callback_value, argument_list) that the evaluator
    /// should call. The caller is responsible for actually invoking them
    /// (to avoid borrow issues with the evaluator).
    ///
    /// Each callback receives: (SYMBOL NEWVAL OPERATION WHERE)
    /// - SYMBOL: the variable name
    /// - NEWVAL: the new value
    /// - OPERATION: one of "set", "let", "unlet", "makunbound", "defvaralias"
    /// - WHERE: location designator (`nil` for global, buffer for buffer-local)
    pub fn notify_watchers(
        &self,
        var_name: &str,
        new_val: &Value,
        _old_val: &Value,
        operation: &str,
        where_val: &Value,
    ) -> Vec<(Value, Vec<Value>)> {
        let mut calls = Vec::new();
        if let Some(list) = self.watchers.get(var_name) {
            for watcher in list {
                let args = vec![
                    Value::symbol(var_name),
                    *new_val,
                    Value::symbol(operation),
                    *where_val,
                ];
                calls.push((watcher.callback, args));
            }
        }
        calls
    }
}

fn watcher_callback_matches(registered: &Value, candidate: &Value) -> bool {
    if registered == candidate {
        return true;
    }
    match (registered, candidate) {
        (Value::Lambda(_), Value::Lambda(_)) | (Value::Macro(_), Value::Macro(_)) => {
            lambda_data_matches(registered, candidate)
        }
        _ => false,
    }
}

fn lambda_data_matches(left: &Value, right: &Value) -> bool {
    match (left.get_lambda_data(), right.get_lambda_data()) {
        (Some(l), Some(r)) => {
            l.params.required == r.params.required
                && l.params.optional == r.params.optional
                && l.params.rest == r.params.rest
                && l.body == r.body
                && lex_envs_equal(&l.env, &r.env)
                && l.docstring == r.docstring
        }
        _ => false,
    }
}

/// Equality for lexical environments (Option<Value>).
fn lex_envs_equal(
    a: &Option<super::value::Value>,
    b: &Option<super::value::Value>,
) -> bool {
    a == b
}

impl Default for VariableWatcherList {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for VariableWatcherList {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for watcher_list in self.watchers.values() {
            for watcher in watcher_list {
                roots.push(watcher.callback);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Builtin functions (eval-dependent)
// ---------------------------------------------------------------------------

use super::error::{signal, EvalResult, Flow};

/// Expect exactly N arguments.
fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Extract a symbol name from a Value.
fn expect_symbol_name(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

/// `(add-variable-watcher SYMBOL WATCH-FUNCTION)`
///
/// Arrange to call WATCH-FUNCTION when SYMBOL is set.
pub(crate) fn builtin_add_variable_watcher(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("add-variable-watcher", &args, 2)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved = super::builtins::resolve_variable_alias_name(eval, &var_name)?;
    let callback = args[1];

    eval.watchers.add_watcher(&resolved, callback);
    Ok(Value::Nil)
}

/// `(remove-variable-watcher SYMBOL WATCH-FUNCTION)`
///
/// Remove WATCH-FUNCTION from the watchers of SYMBOL.
pub(crate) fn builtin_remove_variable_watcher(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("remove-variable-watcher", &args, 2)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved = super::builtins::resolve_variable_alias_name(eval, &var_name)?;
    let callback = args[1];

    eval.watchers.remove_watcher(&resolved, &callback);
    Ok(Value::Nil)
}

/// `(get-variable-watchers SYMBOL)`
///
/// Return a list of watcher callbacks registered for SYMBOL.
pub(crate) fn builtin_get_variable_watchers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-variable-watchers", &args, 1)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved = super::builtins::resolve_variable_alias_name(eval, &var_name)?;
    Ok(Value::list(eval.watchers.get_watchers(&resolved)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::expr::Expr;
    use super::super::intern::intern;
    use super::super::value::{LambdaData, LambdaParams};

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
            &Value::Int(42),
            &Value::Int(0),
            "set",
            &Value::Nil,
        );
        assert_eq!(calls.len(), 1);

        let (callback, args) = &calls[0];
        assert!(matches!(callback, Value::Symbol(id) if resolve_sym(*id) == "my-watcher"));
        assert_eq!(args.len(), 4);
        // arg 0: symbol name
        assert!(matches!(&args[0], Value::Symbol(id) if resolve_sym(*id) == "my-var"));
        // arg 1: new value
        assert!(matches!(&args[1], Value::Int(42)));
        // arg 2: operation
        assert!(matches!(&args[2], Value::Symbol(id) if resolve_sym(*id) == "set"));
        // arg 3: where (nil)
        assert!(matches!(&args[3], Value::Nil));
    }

    #[test]
    fn remove_watcher() {
        let mut wl = VariableWatcherList::new();
        wl.add_watcher("my-var", Value::symbol("watcher1"));
        wl.add_watcher("my-var", Value::symbol("watcher2"));
        assert!(wl.has_watchers("my-var"));

        wl.remove_watcher("my-var", &Value::symbol("watcher1"));
        let calls = wl.notify_watchers("my-var", &Value::Int(1), &Value::Int(0), "set", &Value::Nil);
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0].0, Value::Symbol(id) if resolve_sym(*id) == "watcher2"));
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

        let calls = wl.notify_watchers("my-var", &Value::Int(1), &Value::Int(0), "set", &Value::Nil);
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
        });

        wl.add_watcher("my-var", callback_a);
        wl.add_watcher("my-var", callback_b);
        assert_eq!(wl.get_watchers("my-var"), vec![callback_a]);
    }

    #[test]
    fn notify_no_watchers_returns_empty() {
        let wl = VariableWatcherList::new();
        let calls = wl.notify_watchers("no-var", &Value::Int(1), &Value::Int(0), "set", &Value::Nil);
        assert!(calls.is_empty());
    }

    #[test]
    fn multiple_watchers_all_notified() {
        let mut wl = VariableWatcherList::new();
        wl.add_watcher("v", Value::symbol("w1"));
        wl.add_watcher("v", Value::symbol("w2"));
        wl.add_watcher("v", Value::symbol("w3"));

        let calls = wl.notify_watchers("v", &Value::Int(99), &Value::Int(0), "set", &Value::Nil);
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
        let mut eval = super::super::eval::Evaluator::new();
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
            builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watched-var")])
                .unwrap();
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
            builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watched-var")])
                .unwrap();
        assert_eq!(
            super::super::value::list_to_vec(&remaining).expect("watcher list"),
            vec![Value::symbol("watch-b")]
        );

        let wrong_type = builtin_get_variable_watchers(&mut eval, vec![Value::Int(1)]).unwrap_err();
        match wrong_type {
            Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
            other => panic!("expected signal, got {other:?}"),
        }
    }

    #[test]
    fn variable_watcher_builtins_follow_runtime_alias_resolution() {
        let mut eval = super::super::eval::Evaluator::new();
        super::super::builtins::builtin_defvaralias_eval(
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
        let remaining =
            builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-base")])
                .expect("get-variable-watchers should return empty after removal");
        assert!(remaining.is_nil());
    }

    #[test]
    fn remove_variable_watcher_accepts_non_symbol_callbacks() {
        let mut eval = super::super::eval::Evaluator::new();
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
        });

        builtin_add_variable_watcher(
            &mut eval,
            vec![Value::symbol("vm-watch-nonsym"), callback],
        )
        .expect("add-variable-watcher should accept lambda callbacks");
        let before =
            builtin_get_variable_watchers(&mut eval, vec![Value::symbol("vm-watch-nonsym")])
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
}
