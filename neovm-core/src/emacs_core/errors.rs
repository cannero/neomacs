//! Emacs error hierarchy system.
//!
//! Implements `define-error`, error condition matching via `error-conditions`
//! and `error-message` symbol properties (matching real Emacs behavior), and
//! provides `init_standard_errors` to pre-populate the standard hierarchy.
//!
//! # How it works
//!
//! Each error symbol has two plist properties:
//! - `error-conditions`: a list of symbols representing the error and all its
//!   ancestors (including itself).  E.g. for `file-missing`:
//!   `(file-missing file-error error)`
//! - `error-message`: a human-readable string describing the error.
//!
//! `condition-case` uses `signal_matches_hierarchical` to check whether a
//! signalled error's `error-conditions` list includes the handler's condition
//! symbol.

use super::error::{EvalResult, Flow, signal, signal_with_data};
use super::expr::Expr;
use super::intern::resolve_sym;
use super::symbol::Obarray;
use super::value::*;

// ---------------------------------------------------------------------------
// Obarray-based error hierarchy helpers
// ---------------------------------------------------------------------------

/// Set `error-conditions` and `error-message` properties on `name` in the
/// obarray.  `conditions` is the full list of condition symbols (including
/// `name` itself, its parents, and their transitive ancestors).
fn put_error_properties(obarray: &mut Obarray, name: &str, message: &str, conditions: Vec<&str>) {
    let cond_list = Value::list(conditions.iter().map(|s| Value::symbol(*s)).collect());
    obarray.put_property(name, "error-conditions", cond_list);
    obarray.put_property(name, "error-message", Value::string(message));
}

/// Collect the full condition list for `name` given its direct `parents`.
/// The result always starts with `name`, then the union of each parent's
/// `error-conditions` list (read from the obarray).  If a parent has no
/// `error-conditions` yet, just the parent symbol itself is included.
fn build_conditions_from_obarray(obarray: &Obarray, name: &str, parents: &[&str]) -> Vec<String> {
    let mut conditions = vec![name.to_string()];
    for &parent in parents {
        // Read the parent's error-conditions list from the obarray.
        if let Some(parent_conds) = obarray.get_property(parent, "error-conditions") {
            for sym in iter_symbol_list(parent_conds) {
                if !conditions.contains(&sym) {
                    conditions.push(sym);
                }
            }
        } else {
            // Parent not yet registered — include the bare symbol.
            if !conditions.contains(&parent.to_string()) {
                conditions.push(parent.to_string());
            }
        }
    }
    conditions
}

/// Iterate over a Value list, yielding symbol names.
fn iter_symbol_list(value: &Value) -> Vec<String> {
    let mut result = Vec::new();
    if let Some(items) = list_to_vec(value) {
        for item in items {
            if let Some(name) = item.as_symbol_name() {
                result.push(name.to_string());
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Hierarchical signal matching (for condition-case)
// ---------------------------------------------------------------------------

/// Check whether `signal_sym` matches `condition_sym` using the error
/// hierarchy stored in the obarray.
///
/// Returns `true` if:
/// - `condition_sym` is `"t"` (catches everything),
/// - `condition_sym == signal_sym`, or
/// - `condition_sym` appears in `signal_sym`'s `error-conditions` plist.
///
/// This is the hierarchical replacement for the flat `signal_matches` in
/// `error.rs`.
pub fn signal_matches_hierarchical(
    obarray: &Obarray,
    signal_sym: &str,
    condition_sym: &str,
) -> bool {
    // `t` catches all signals.
    if condition_sym == "t" {
        return true;
    }
    // Exact match (fast path).
    if signal_sym == condition_sym {
        return true;
    }
    // Check the error-conditions plist on the signal symbol.
    if let Some(conds) = obarray.get_property(signal_sym, "error-conditions") {
        for sym_name in iter_symbol_list(conds) {
            if sym_name == condition_sym {
                return true;
            }
        }
    }
    false
}

/// Like `signal_matches_hierarchical` but accepts a condition pattern that may
/// be a single symbol or a list of symbols (as used by `condition-case`
/// handler heads).
pub fn signal_matches_condition_pattern(
    obarray: &Obarray,
    signal_sym: &str,
    pattern: &Expr,
) -> bool {
    match pattern {
        Expr::Symbol(id) => signal_matches_hierarchical(obarray, signal_sym, resolve_sym(*id)),
        Expr::List(items) => items
            .iter()
            .any(|item| signal_matches_condition_pattern(obarray, signal_sym, item)),
        _ => false,
    }
}

/// Like `signal_matches_condition_pattern`, but matches a runtime `Value`
/// produced by compiled bytecode condition handlers.
pub fn signal_matches_condition_value(
    obarray: &Obarray,
    signal_sym: &str,
    pattern: &Value,
) -> bool {
    match pattern {
        Value::Symbol(id) | Value::Keyword(id) => {
            signal_matches_hierarchical(obarray, signal_sym, resolve_sym(*id))
        }
        Value::True => true,
        Value::Nil => false,
        Value::Cons(_) => list_to_vec(pattern).is_some_and(|items| {
            items
                .iter()
                .any(|item| signal_matches_condition_value(obarray, signal_sym, item))
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Standard Emacs error hierarchy initialisation
// ---------------------------------------------------------------------------

/// Pre-populate the obarray with the standard Emacs error hierarchy.
///
/// Must be called once during evaluator initialisation (after the obarray is
/// created but before any user code runs).
pub fn init_standard_errors(obarray: &mut Obarray) {
    // Root error.
    put_error_properties(obarray, "error", "error", vec!["error"]);

    // --- Direct children of `error` ---

    register_simple(obarray, "quit", "Quit", &["error"]);
    register_simple(obarray, "user-error", "User error", &["error"]);
    register_simple(
        obarray,
        "args-out-of-range",
        "Args out of range",
        &["error"],
    );
    register_simple(
        obarray,
        "beginning-of-buffer",
        "Beginning of buffer",
        &["error"],
    );
    register_simple(obarray, "end-of-buffer", "End of buffer", &["error"]);
    register_simple(
        obarray,
        "end-of-file",
        "End of file during parsing",
        &["error"],
    );
    register_simple(
        obarray,
        "buffer-read-only",
        "Buffer is read-only",
        &["error"],
    );
    register_simple(
        obarray,
        "coding-system-error",
        "Invalid coding system",
        &["error"],
    );
    register_simple(
        obarray,
        "cyclic-function-indirection",
        "Symbol's chain of function indirections contains a loop",
        &["error"],
    );
    register_simple(
        obarray,
        "cyclic-variable-indirection",
        "Symbol's chain of variable indirections contains a loop",
        &["error"],
    );
    register_simple(obarray, "invalid-function", "Invalid function", &["error"]);
    register_simple(
        obarray,
        "invalid-read-syntax",
        "Invalid read syntax",
        &["error"],
    );
    register_simple(obarray, "invalid-regexp", "Invalid regexp", &["error"]);
    register_simple(
        obarray,
        "wrong-length-argument",
        "Wrong length argument",
        &["error"],
    );
    register_simple(
        obarray,
        "mark-inactive",
        "The mark is not active now",
        &["error"],
    );
    register_simple(obarray, "no-catch", "No catch for tag", &["error"]);
    register_simple(obarray, "scan-error", "Scan error", &["error"]);
    register_simple(obarray, "search-failed", "Search failed", &["error"]);
    register_simple(
        obarray,
        "setting-constant",
        "Attempt to set a constant symbol",
        &["error"],
    );
    register_simple(obarray, "text-read-only", "Text is read-only", &["error"]);
    register_simple(
        obarray,
        "void-function",
        "Symbol\u{2019}s function definition is void",
        &["error"],
    );
    register_simple(
        obarray,
        "void-variable",
        "Symbol\u{2019}s value as variable is void",
        &["error"],
    );
    register_simple(
        obarray,
        "wrong-number-of-arguments",
        "Wrong number of arguments",
        &["error"],
    );
    register_simple(
        obarray,
        "wrong-type-argument",
        "Wrong type argument",
        &["error"],
    );
    register_simple(
        obarray,
        "cl-assertion-failed",
        "Assertion failed",
        &["error"],
    );
    // GNU fns.c — type-mismatch is signaled by value< for incompatible types.
    register_simple(obarray, "type-mismatch", "Type mismatch", &["error"]);
    register_simple(
        obarray,
        "permission-denied",
        "Permission denied",
        &["error"],
    );
    register_simple(
        obarray,
        "recursion-error",
        "Excessive recursive calling error",
        &["error"],
    );

    // --- arith-error family ---
    register_simple(obarray, "arith-error", "Arithmetic error", &["error"]);
    register_simple(
        obarray,
        "overflow-error",
        "Arithmetic overflow error",
        &["arith-error"],
    );
    register_simple(
        obarray,
        "range-error",
        "Arithmetic range error",
        &["arith-error"],
    );
    register_simple(
        obarray,
        "domain-error",
        "Arithmetic domain error",
        &["arith-error"],
    );
    register_simple(
        obarray,
        "underflow-error",
        "Arithmetic underflow error",
        &["arith-error"],
    );

    // --- file-error family ---
    register_simple(obarray, "file-error", "File error", &["error"]);
    register_simple(
        obarray,
        "file-already-exists",
        "File already exists",
        &["file-error"],
    );
    register_simple(
        obarray,
        "file-date-error",
        "Cannot set file date",
        &["file-error"],
    );
    register_simple(obarray, "file-locked", "File is locked", &["file-error"]);
    register_simple(obarray, "file-missing", "File is missing", &["file-error"]);
    register_simple(
        obarray,
        "file-notify-error",
        "File notification error",
        &["file-error"],
    );

    // --- json-error family ---
    register_simple(obarray, "json-error", "JSON error", &["error"]);
    register_simple(
        obarray,
        "json-parse-error",
        "JSON parse error",
        &["json-error"],
    );
    register_simple(
        obarray,
        "json-serialize-error",
        "JSON serialize error",
        &["json-error"],
    );

    // --- remote-file-error (child of file-error) ---
    register_simple(
        obarray,
        "remote-file-error",
        "Remote file error",
        &["file-error"],
    );

    // Also register some common signal names that may be used without a
    // full `define-error` (e.g. excessive-lisp-nesting).
    register_simple(
        obarray,
        "excessive-lisp-nesting",
        "Lisp nesting exceeds `max-lisp-eval-depth'",
        &["recursion-error"],
    );
}

/// Helper: register a single error with explicit parents.
/// The parents must already be registered in the obarray (their
/// `error-conditions` are read to build the transitive closure).
fn register_simple(obarray: &mut Obarray, name: &str, message: &str, parents: &[&str]) {
    let conditions = build_conditions_from_obarray(obarray, name, parents);
    let cond_refs: Vec<&str> = conditions.iter().map(|s| s.as_str()).collect();
    put_error_properties(obarray, name, message, cond_refs);
}

/// Extract parent symbol(s) from the PARENT argument of `define-error`.
/// Accepts either a single symbol or a list of symbols.
fn extract_parent_symbols(value: &Value) -> Result<Vec<String>, Flow> {
    match value {
        Value::Symbol(id) => Ok(vec![resolve_sym(*id).to_owned()]),
        Value::Nil => Ok(vec!["error".to_string()]),
        Value::True => Ok(vec!["t".to_string()]),
        Value::Cons(_) => {
            let items = list_to_vec(value).ok_or_else(|| {
                signal("wrong-type-argument", vec![Value::symbol("listp"), *value])
            })?;
            let mut parents = Vec::with_capacity(items.len());
            for item in &items {
                match item.as_symbol_name() {
                    Some(name) => parents.push(name.to_string()),
                    None => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("symbolp"), *item],
                        ));
                    }
                }
            }
            if parents.is_empty() {
                Ok(vec!["error".to_string()])
            } else {
                Ok(parents)
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Builtins: signal wrapper and error-message-string
// ---------------------------------------------------------------------------

/// Eval-aware `signal` — checks error hierarchy and converts
/// unregistered error symbols to `(error "Invalid error symbol" SYM)`,
/// matching GNU eval.c:1949-1951.
pub(crate) fn builtin_signal(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() != 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("signal"), Value::Int(args.len() as i64)],
        ));
    }

    let sym_name = match args[0].as_symbol_name() {
        Some(name) => name.to_string(),
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };

    // GNU eval.c:1949-1951: check error-conditions property.
    // If the symbol has no error-conditions (not registered in the
    // error hierarchy), convert to (error "Invalid error symbol" SYM).
    if sym_name != "error" && sym_name != "quit" {
        let has_conditions = eval
            .obarray
            .get_property(&sym_name, "error-conditions")
            .is_some();
        if !has_conditions {
            return Err(signal(
                "error",
                vec![Value::string("Invalid error symbol"), args[0]],
            ));
        }
    }

    let flow = match &args[1] {
        Value::Nil => signal(&sym_name, vec![]),
        Value::Cons(_) => match list_to_vec(&args[1]) {
            Some(data) => signal(&sym_name, data),
            None => signal_with_data(&sym_name, args[1]),
        },
        _ => signal_with_data(&sym_name, args[1]),
    };

    Err(flow)
}

/// `(error-message-string ERROR-DATA)` — format an error for display.
///
/// ERROR-DATA is `(ERROR-SYMBOL . DATA)` as bound by `condition-case`.
/// Looks up `error-message` on the symbol's plist and appends the data.
pub(crate) fn builtin_error_message_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() != 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("error-message-string"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let error_data = &args[0];

    // Emacs expects ERROR-DATA to be a list (or nil).
    let (sym_name, data) = match error_data {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            let sym = match pair.car.as_symbol_name() {
                Some(name) => name.to_string(),
                None => return Ok(Value::string("peculiar error")),
            };
            let rest = match &pair.cdr {
                Value::Nil => vec![],
                Value::Cons(_) => list_to_vec(&pair.cdr).unwrap_or_else(|| vec![pair.cdr]),
                other => vec![*other],
            };
            (sym, rest)
        }
        Value::Nil => return Ok(Value::string("peculiar error")),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *error_data],
            ));
        }
    };

    // Look up the error-message property.
    let base_message = eval
        .obarray
        .get_property(&sym_name, "error-message")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| sym_name.clone());
    let is_known_error = signal_matches_hierarchical(&eval.obarray, &sym_name, "error");

    // Unknown condition symbols are formatted as peculiar errors.
    if !is_known_error {
        if data.is_empty() {
            return Ok(Value::string("peculiar error"));
        }
        let data_strs: Vec<String> = data
            .iter()
            .map(|v| format_error_arg(eval, v, true))
            .collect();
        return Ok(Value::string(format!(
            "peculiar error: {}",
            data_strs.join(", ")
        )));
    }

    if data.is_empty() {
        if sym_name == "error" {
            return Ok(Value::string("peculiar error"));
        }
        if sym_name == "user-error" {
            return Ok(Value::string(""));
        }
        return Ok(Value::string(base_message));
    }

    // `user-error` always renders payload data directly.
    if sym_name == "user-error" {
        if let Some(first_str) = data.first().and_then(|v| v.as_str().map(|s| s.to_string())) {
            let rest = &data[1..];
            if rest.is_empty() {
                return Ok(Value::string(first_str));
            }
            let rest_strs: Vec<String> = rest
                .iter()
                .map(|v| format_error_arg(eval, v, false))
                .collect();
            return Ok(Value::string(format!(
                "{first_str}, {}",
                rest_strs.join(", ")
            )));
        }
        let data_strs: Vec<String> = data
            .iter()
            .map(|v| format_error_arg(eval, v, false))
            .collect();
        return Ok(Value::string(data_strs.join(", ")));
    }

    let is_file_error_family = signal_matches_hierarchical(&eval.obarray, &sym_name, "file-error");
    let is_file_locked = sym_name == "file-locked";

    // `file-locked` is an oddball in Emacs: it always reports "peculiar error"
    // with all payload elements, even if the first datum is a string.
    if is_file_locked {
        let data_strs: Vec<String> = data
            .iter()
            .map(|v| format_error_arg(eval, v, true))
            .collect();
        return Ok(Value::string(format!(
            "peculiar error: {}",
            data_strs.join(", ")
        )));
    }

    // `error` and file-error-family conditions use a leading string for
    // user-facing detail.
    if sym_name == "error" || is_file_error_family {
        if let Some(first_str) = data.first().and_then(|v| v.as_str().map(|s| s.to_string())) {
            let rest = &data[1..];
            if rest.is_empty() {
                return Ok(Value::string(first_str));
            }
            let quote_strings = sym_name == "error";
            let rest_strs: Vec<String> = rest
                .iter()
                .map(|v| format_error_arg(eval, v, quote_strings))
                .collect();
            return Ok(Value::string(format!(
                "{first_str}: {}",
                rest_strs.join(", ")
            )));
        }

        // `error` and most file-error-family members render peculiar payload
        // data from the second element onward when no leading message string
        // is present.
        if data.len() > 1 {
            let detail: Vec<String> = data[1..]
                .iter()
                .map(|v| format_error_arg(eval, v, true))
                .collect();
            return Ok(Value::string(format!(
                "peculiar error: {}",
                detail.join(", ")
            )));
        }
        return Ok(Value::string("peculiar error"));
    }

    let quote_strings = sym_name != "end-of-file";
    let data_strs: Vec<String> = data
        .iter()
        .map(|v| format_error_arg(eval, v, quote_strings))
        .collect();
    Ok(Value::string(format!(
        "{}: {}",
        base_message,
        data_strs.join(", ")
    )))
}

fn format_error_arg(eval: &super::eval::Context, value: &Value, quote_strings: bool) -> String {
    if !quote_strings {
        if let Some(s) = value.as_str() {
            return s.to_string();
        }
    }
    super::error::print_value_with_eval(eval, value)
}

// ---------------------------------------------------------------------------
// ErrorRegistry (HashMap-based, standalone — usable without an Obarray)
// ---------------------------------------------------------------------------

/// A standalone registry that tracks error parent relationships.
///
/// This can be used independently of the obarray (e.g. for testing or
/// embedding).  For the full Emacs-compatible approach, prefer the
/// obarray-based functions above.
pub struct ErrorRegistry {
    /// Map from error symbol name to its parent error symbol names.
    parents: std::collections::HashMap<String, Vec<String>>,
}

impl ErrorRegistry {
    /// Create a new registry pre-populated with the standard Emacs error
    /// hierarchy.
    pub fn new() -> Self {
        let mut reg = Self {
            parents: std::collections::HashMap::new(),
        };
        reg.init_standard_hierarchy();
        reg
    }

    /// Register a new error type.
    pub fn define_error(&mut self, name: &str, _message: &str, parents: &[&str]) {
        let parent_list = if parents.is_empty() {
            vec!["error".to_string()]
        } else {
            parents.iter().map(|s| s.to_string()).collect()
        };
        self.parents.insert(name.to_string(), parent_list);
    }

    /// Check whether `signal` inherits from `condition` (directly or
    /// transitively).
    pub fn signal_matches_condition(&self, signal_sym: &str, condition: &str) -> bool {
        if condition == "t" {
            return true;
        }
        if signal_sym == condition {
            return true;
        }
        // BFS/DFS through parents.
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![signal_sym.to_string()];
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(parents) = self.parents.get(&current) {
                for parent in parents {
                    if parent == condition {
                        return true;
                    }
                    stack.push(parent.clone());
                }
            }
        }
        false
    }

    /// Collect the full condition list for a signal (self + all ancestors).
    pub fn conditions_for(&self, signal_sym: &str) -> Vec<String> {
        let mut result = vec![signal_sym.to_string()];
        let mut visited = std::collections::HashSet::new();
        visited.insert(signal_sym.to_string());
        let mut stack = vec![signal_sym.to_string()];
        while let Some(current) = stack.pop() {
            if let Some(parents) = self.parents.get(&current) {
                for parent in parents {
                    if visited.insert(parent.clone()) {
                        result.push(parent.clone());
                        stack.push(parent.clone());
                    }
                }
            }
        }
        result
    }

    fn init_standard_hierarchy(&mut self) {
        // Root.
        self.parents.insert("error".to_string(), vec![]);

        let simple_children_of_error = [
            "quit",
            "user-error",
            "args-out-of-range",
            "beginning-of-buffer",
            "end-of-buffer",
            "buffer-read-only",
            "coding-system-error",
            "invalid-function",
            "invalid-read-syntax",
            "invalid-regexp",
            "mark-inactive",
            "no-catch",
            "scan-error",
            "search-failed",
            "setting-constant",
            "text-read-only",
            "void-function",
            "void-variable",
            "wrong-number-of-arguments",
            "wrong-type-argument",
            "cl-assertion-failed",
            "permission-denied",
            "recursion-error",
        ];
        for name in &simple_children_of_error {
            self.parents
                .insert(name.to_string(), vec!["error".to_string()]);
        }

        // arith-error family.
        self.parents
            .insert("arith-error".to_string(), vec!["error".to_string()]);
        for name in &[
            "overflow-error",
            "range-error",
            "domain-error",
            "underflow-error",
        ] {
            self.parents
                .insert(name.to_string(), vec!["arith-error".to_string()]);
        }

        // file-error family.
        self.parents
            .insert("file-error".to_string(), vec!["error".to_string()]);
        for name in &[
            "file-already-exists",
            "file-date-error",
            "file-locked",
            "file-missing",
            "file-notify-error",
        ] {
            self.parents
                .insert(name.to_string(), vec!["file-error".to_string()]);
        }

        // json-error family.
        self.parents
            .insert("json-error".to_string(), vec!["error".to_string()]);
        for name in &["json-parse-error", "json-serialize-error"] {
            self.parents
                .insert(name.to_string(), vec!["json-error".to_string()]);
        }

        // remote-file-error is a child of file-error.
        self.parents.insert(
            "remote-file-error".to_string(),
            vec!["file-error".to_string()],
        );
    }
}

impl Default for ErrorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "errors_test.rs"]
mod tests;
