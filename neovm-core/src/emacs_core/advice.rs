//! Variable watchers for the Elisp VM.
//!
//! Provides callbacks invoked when a watched variable changes
//! (like Emacs `add-variable-watcher` / `remove-variable-watcher`).

use std::collections::HashMap;

use super::intern::resolve_sym;
use super::symbol::Obarray;
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
            .map(|list| list.iter().map(|watcher| watcher.callback).collect())
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

    // pdump accessors
    pub(crate) fn dump_watchers(&self) -> &HashMap<String, Vec<VariableWatcher>> {
        &self.watchers
    }
    pub(crate) fn from_dump(watchers: HashMap<String, Vec<VariableWatcher>>) -> Self {
        Self { watchers }
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
fn lex_envs_equal(a: &Option<super::value::Value>, b: &Option<super::value::Value>) -> bool {
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

use super::error::{EvalResult, Flow, signal};

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
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("add-variable-watcher", &args, 2)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved =
        super::builtins::resolve_variable_alias_name_in_obarray(eval.obarray(), &var_name)?;
    let callback = args[1];

    eval.watchers.add_watcher(&resolved, callback);
    Ok(Value::Nil)
}

/// `(remove-variable-watcher SYMBOL WATCH-FUNCTION)`
///
/// Remove WATCH-FUNCTION from the watchers of SYMBOL.
pub(crate) fn builtin_remove_variable_watcher(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("remove-variable-watcher", &args, 2)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved =
        super::builtins::resolve_variable_alias_name_in_obarray(eval.obarray(), &var_name)?;
    let callback = args[1];

    eval.watchers.remove_watcher(&resolved, &callback);
    Ok(Value::Nil)
}

/// `(get-variable-watchers SYMBOL)`
///
/// Return a list of watcher callbacks registered for SYMBOL.
pub(crate) fn builtin_get_variable_watchers(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_get_variable_watchers_in_state(eval.obarray(), &eval.watchers, args)
}

pub(crate) fn builtin_get_variable_watchers_in_state(
    obarray: &Obarray,
    watchers: &VariableWatcherList,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-variable-watchers", &args, 1)?;

    let var_name = expect_symbol_name(&args[0])?;
    let resolved = super::builtins::resolve_variable_alias_name_in_obarray(obarray, &var_name)?;
    Ok(Value::list(watchers.get_watchers(&resolved)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "advice_test.rs"]
mod tests;
