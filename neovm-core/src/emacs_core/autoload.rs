//! Autoload, compile-time evaluation, obsolete function/variable support.
//!
//! Provides:
//! - **Autoload system**: Deferred function loading — register a function as
//!   autoloaded so that its file is loaded on first use.
//! - **eval-when-compile / eval-and-compile**: Compile-time evaluation stubs
//!   (in the interpreter they just evaluate normally).
//! - **with-eval-after-load**: Deferred form execution after a file loads.
//! - **Obsolete aliases**: `define-obsolete-function-alias`,
//!   `define-obsolete-variable-alias`, `make-obsolete`, `make-obsolete-variable`.

use std::collections::HashMap;
use std::path::PathBuf;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::symbol::Obarray;
use super::value::*;
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Autoload types
// ---------------------------------------------------------------------------

/// The kind of definition an autoload stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoloadType {
    /// Normal function (default).
    Function,
    /// Macro.
    Macro,
    /// Keymap.
    Keymap,
}

impl AutoloadType {
    /// Parse a Value into an AutoloadType.
    pub fn from_value(val: &Value) -> Self {
        if matches!(val, Value::True) {
            return Self::Macro;
        }
        match val.as_symbol_name() {
            Some("macro") => Self::Macro,
            Some("keymap") => Self::Keymap,
            _ => Self::Function,
        }
    }

    /// Convert back to a symbol Value.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Function => Value::Nil,
            Self::Macro => Value::symbol("macro"),
            Self::Keymap => Value::symbol("keymap"),
        }
    }
}

/// An entry in the autoload table.
#[derive(Clone, Debug)]
pub struct AutoloadEntry {
    /// The function name that is autoloaded.
    pub name: String,
    /// The file to load when the function is first called.
    pub file: String,
    /// Optional documentation string.
    pub docstring: Option<String>,
    /// Whether the function is interactive (a command).
    pub interactive: bool,
    /// The type of definition (function, macro, keymap).
    pub autoload_type: AutoloadType,
}

// ---------------------------------------------------------------------------
// AutoloadManager
// ---------------------------------------------------------------------------

/// Central registry of autoloaded functions and eval-after-load callbacks.
pub struct AutoloadManager {
    /// Map from function name to autoload entry.
    entries: HashMap<String, AutoloadEntry>,
    /// Map from file/feature name to list of forms to evaluate after loading.
    after_load: HashMap<String, Vec<Value>>,
    /// Set of files that have already been loaded (for after-load tracking).
    loaded_files: Vec<String>,
    /// Obsolete function warnings: old-name -> (new-name, when).
    obsolete_functions: HashMap<String, (String, String)>,
    /// Obsolete variable warnings: old-name -> (new-name, when).
    obsolete_variables: HashMap<String, (String, String)>,
}

impl Default for AutoloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoloadManager {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            after_load: HashMap::new(),
            loaded_files: Vec::new(),
            obsolete_functions: HashMap::new(),
            obsolete_variables: HashMap::new(),
        }
    }

    /// Register an autoload entry.
    pub fn register(&mut self, entry: AutoloadEntry) {
        self.entries.insert(entry.name.clone(), entry);
    }

    /// Check whether a function name has an autoload entry.
    pub fn is_autoloaded(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Get the autoload entry for a function name.
    pub fn get_entry(&self, name: &str) -> Option<&AutoloadEntry> {
        self.entries.get(name)
    }

    /// Snapshot current autoload entries for callers that need to rebuild
    /// function cells from the registered autoload metadata.
    pub fn entries_snapshot(&self) -> Vec<AutoloadEntry> {
        self.entries.values().cloned().collect()
    }

    /// Remove an autoload entry (used after the file has been loaded and the
    /// real definition is in place).
    pub fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Register a form to evaluate after a given file/feature is loaded.
    pub fn add_after_load(&mut self, file: &str, form: Value) {
        self.after_load
            .entry(file.to_string())
            .or_default()
            .push(form);
    }

    /// Get the after-load forms for a file (if any).
    pub fn take_after_load_forms(&mut self, file: &str) -> Vec<Value> {
        self.after_load.remove(file).unwrap_or_default()
    }

    /// Record that a file has been loaded.
    pub fn mark_loaded(&mut self, file: &str) {
        if !self.loaded_files.contains(&file.to_string()) {
            self.loaded_files.push(file.to_string());
        }
    }

    /// Check if a file has already been loaded.
    pub fn is_loaded(&self, file: &str) -> bool {
        self.loaded_files.contains(&file.to_string())
    }

    /// Mark a function as obsolete.
    pub fn make_obsolete(&mut self, old_name: &str, new_name: &str, when: &str) {
        self.obsolete_functions.insert(
            old_name.to_string(),
            (new_name.to_string(), when.to_string()),
        );
    }

    /// Check if a function is marked obsolete.
    pub fn is_function_obsolete(&self, name: &str) -> bool {
        self.obsolete_functions.contains_key(name)
    }

    /// Get obsolete function info: (new-name, when).
    pub fn get_obsolete_function(&self, name: &str) -> Option<&(String, String)> {
        self.obsolete_functions.get(name)
    }

    /// Mark a variable as obsolete.
    pub fn make_variable_obsolete(&mut self, old_name: &str, new_name: &str, when: &str) {
        self.obsolete_variables.insert(
            old_name.to_string(),
            (new_name.to_string(), when.to_string()),
        );
    }

    /// Check if a variable is marked obsolete.
    pub fn is_variable_obsolete(&self, name: &str) -> bool {
        self.obsolete_variables.contains_key(name)
    }

    /// Get obsolete variable info: (new-name, when).
    pub fn get_obsolete_variable(&self, name: &str) -> Option<&(String, String)> {
        self.obsolete_variables.get(name)
    }

    // pdump accessors
    pub(crate) fn dump_entries(&self) -> &HashMap<String, AutoloadEntry> {
        &self.entries
    }
    pub(crate) fn dump_after_load(&self) -> &HashMap<String, Vec<Value>> {
        &self.after_load
    }
    pub(crate) fn dump_loaded_files(&self) -> &[String] {
        &self.loaded_files
    }
    pub(crate) fn dump_obsolete_functions(&self) -> &HashMap<String, (String, String)> {
        &self.obsolete_functions
    }
    pub(crate) fn dump_obsolete_variables(&self) -> &HashMap<String, (String, String)> {
        &self.obsolete_variables
    }
    pub(crate) fn from_dump(
        entries: HashMap<String, AutoloadEntry>,
        after_load: HashMap<String, Vec<Value>>,
        loaded_files: Vec<String>,
        obsolete_functions: HashMap<String, (String, String)>,
        obsolete_variables: HashMap<String, (String, String)>,
    ) -> Self {
        Self {
            entries,
            after_load,
            loaded_files,
            obsolete_functions,
            obsolete_variables,
        }
    }
}

// ---------------------------------------------------------------------------
// Builtins (pure — need evaluator access)
// ---------------------------------------------------------------------------

/// Check whether a value is an autoload form (autoload FILE ...).
pub(crate) fn is_autoload_value(val: &Value) -> bool {
    if let Some(items) = list_to_vec(val) {
        if let Some(first) = items.first() {
            if let Some(name) = first.as_symbol_name() {
                return name == "autoload";
            }
        }
    }
    false
}

/// `(autoload-do-load FUNDEF &optional FUNNAME MACRO-ONLY)` — trigger autoload.
/// If FUNDEF is an autoload form, load the file and return the new definition.
/// Otherwise return FUNDEF unchanged.
pub(crate) enum AutoloadDoLoadPlan {
    Return(Value),
    Load {
        file: String,
        funname: Option<String>,
    },
}

pub(crate) fn plan_autoload_do_load_in_state(
    obarray: &Obarray,
    args: &[Value],
) -> Result<AutoloadDoLoadPlan, Flow> {
    if args.is_empty() || args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("autoload-do-load"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let fundef = &args[0];
    if !is_autoload_value(fundef) {
        return Ok(AutoloadDoLoadPlan::Return(*fundef));
    }

    let items = list_to_vec(fundef).unwrap_or_default();
    // items[0] = 'autoload, items[1] = file, ...
    let file = if items.len() > 1 {
        match &items[1] {
            Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
            _ => return Ok(AutoloadDoLoadPlan::Return(*fundef)),
        }
    } else {
        return Ok(AutoloadDoLoadPlan::Return(*fundef));
    };

    let funname = if args.len() > 1 {
        args[1].as_symbol_name().map(|s| s.to_string())
    } else {
        None
    };

    // MACRO-ONLY check: if the third arg is non-nil, only autoload if the
    // autoload's TYPE field (5th element) is `t` or `macro`.
    // This matches GNU Emacs eval.c:Fautoload_do_load.
    let macro_only = args.get(2).copied().unwrap_or(Value::Nil);
    if !macro_only.is_nil() {
        let kind = items.get(4).copied().unwrap_or(Value::Nil);
        let is_macro_type =
            matches!(kind, Value::True) || kind.as_symbol_name().map_or(false, |s| s == "macro");
        if !is_macro_type {
            return Ok(AutoloadDoLoadPlan::Return(*fundef));
        }
    }

    // Before loading, check if the function cell has already been resolved
    // (i.e., a previous load of the same file already defined this function).
    // This prevents redundant re-loads that can cause side effects like
    // advice being installed multiple times.
    if let Some(ref name) = funname {
        if let Some(current) = obarray.symbol_function(name).cloned() {
            if !is_autoload_value(&current) {
                // The function is already defined (not an autoload) — a previous
                // load already resolved it. Return the current definition.
                return Ok(AutoloadDoLoadPlan::Return(current));
            }
        }
    }

    Ok(AutoloadDoLoadPlan::Load { file, funname })
}

pub(crate) fn resolve_autoload_load_path(obarray: &Obarray, file: &str) -> Result<PathBuf, Flow> {
    let load_path = super::load::get_load_path(obarray);
    match super::load::find_file_in_load_path(file, &load_path) {
        Some(path) => Ok(path),
        None => Err(signal(
            "file-missing",
            vec![Value::string(format!(
                "Cannot open load file: no such file or directory, {}",
                file
            ))],
        )),
    }
}

pub(crate) fn finish_autoload_do_load_in_state(
    obarray: &Obarray,
    funname: Option<&str>,
) -> EvalResult {
    if let Some(name) = funname {
        if let Some(func) = obarray.symbol_function(name).cloned() {
            return Ok(func);
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_autoload_do_load(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    match plan_autoload_do_load_in_state(&eval.obarray, &args)? {
        AutoloadDoLoadPlan::Return(value) => Ok(value),
        AutoloadDoLoadPlan::Load { file, funname } => {
            let path = resolve_autoload_load_path(&eval.obarray, &file)?;
            eval.load_file_internal(&path)?;
            finish_autoload_do_load_in_state(&eval.obarray, funname.as_deref())
        }
    }
}

pub(crate) fn builtin_autoload_do_load_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    args: &[Value],
    extra_roots: &[Value],
) -> EvalResult {
    match plan_autoload_do_load_in_state(&*shared.obarray, args)? {
        AutoloadDoLoadPlan::Return(value) => Ok(value),
        AutoloadDoLoadPlan::Load { file, funname } => {
            let path = resolve_autoload_load_path(&*shared.obarray, &file)?;
            shared.with_parent_evaluator_vm_roots(vm_gc_roots, extra_roots, move |eval| {
                eval.load_file_internal(&path)
            })?;
            finish_autoload_do_load_in_state(&*shared.obarray, funname.as_deref())
        }
    }
}

pub(crate) fn register_autoload_in_state(
    obarray: &mut Obarray,
    autoloads: &mut AutoloadManager,
    args: &[Value],
) -> EvalResult {
    if args.len() < 2 || args.len() > 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("autoload"), Value::Int(args.len() as i64)],
        ));
    }

    let func_val = args[0];
    let name = match &func_val {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), func_val],
            ));
        }
    };

    // GNU Emacs eval.c:Fautoload — "If function is defined and not as an
    // autoload, don't override."  If the symbol already has a real (non-
    // autoload) function definition, return nil without touching it.
    if let Some(current) = obarray.symbol_function(&name).cloned() {
        if !current.is_nil() && !is_autoload_value(&current) {
            return Ok(Value::Nil);
        }
    }

    let file_val = args[1];
    let file = match &file_val {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), file_val],
            ));
        }
    };

    let docstring_val = args.get(2).cloned().unwrap_or(Value::Nil);
    let docstring = match &docstring_val {
        Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    };

    let interactive_val = args.get(3).cloned().unwrap_or(Value::Nil);
    let interactive = !matches!(interactive_val, Value::Nil);

    let type_val = args.get(4).cloned().unwrap_or(Value::Nil);
    let autoload_type = AutoloadType::from_value(&type_val);

    let autoload_form = Value::list(vec![
        Value::symbol("autoload"),
        Value::string(file.clone()),
        docstring_val,
        interactive_val,
        type_val,
    ]);

    obarray.set_symbol_function(&name, autoload_form);
    autoloads.register(AutoloadEntry {
        name: name.clone(),
        file,
        docstring,
        interactive,
        autoload_type,
    });

    Ok(Value::symbol(&name))
}

/// `(autoload FUNCTION FILE &optional DOCSTRING INTERACTIVE TYPE)`
///
/// Callable builtin form used by `funcall`/`apply` and direct function calls.
/// Arguments are already evaluated.
pub(crate) fn builtin_autoload(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    register_autoload_in_state(&mut eval.obarray, &mut eval.autoloads, &args)
}

/// `(symbol-file SYMBOL &optional TYPE)` — return the file that defined SYMBOL.
/// Stub: always returns nil for now.
pub(crate) fn builtin_symbol_file(args: Vec<Value>) -> EvalResult {
    if args.is_empty() || args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("symbol-file"), Value::Int(args.len() as i64)],
        ));
    }
    // Stub: we don't track symbol origins yet.
    Ok(Value::Nil)
}

pub(crate) fn builtin_symbol_file_in_state(
    obarray: &Obarray,
    autoloads: &AutoloadManager,
    args: &[Value],
) -> EvalResult {
    if args.is_empty() || args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("symbol-file"), Value::Int(args.len() as i64)],
        ));
    }

    let symbol_name = match args[0].as_symbol_name() {
        Some(name) => name,
        None => return Ok(Value::Nil),
    };

    let function_origin_requested = if args.len() == 1 || matches!(args[1], Value::Nil) {
        true
    } else {
        matches!(args[1].as_symbol_name(), Some("defun"))
    };
    if !function_origin_requested {
        return Ok(Value::Nil);
    }

    if let Some(entry) = autoloads.get_entry(symbol_name) {
        return Ok(Value::string(entry.file.clone()));
    }

    if let Some(fndef) = obarray.symbol_function(symbol_name).cloned() {
        if is_autoload_value(&fndef) {
            if let Some(items) = list_to_vec(&fndef) {
                if let Some(Value::Str(id)) = items.get(1) {
                    return Ok(Value::string(with_heap(|h| h.get_string(*id).to_owned())));
                }
            }
        }
    }

    Ok(Value::Nil)
}

/// Evaluator-aware `(symbol-file SYMBOL &optional TYPE)`.
///
/// NeoVM currently tracks symbol origin only for autoloaded function symbols.
/// This matches GNU Emacs behavior for the currently supported subset:
/// - non-symbol SYMBOL returns nil
/// - TYPE nil/missing/`defun` queries function definition origin
/// - other TYPE values return nil
pub(crate) fn builtin_symbol_file_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_symbol_file_in_state(&eval.obarray, &eval.autoloads, &args)
}

// ---------------------------------------------------------------------------
// Special form handlers (called from eval.rs try_special_form dispatch)
// ---------------------------------------------------------------------------

/// `(autoload FUNCTION FILE &optional DOCSTRING INTERACTIVE TYPE)`
///
/// Register FUNCTION to be autoloaded from FILE.  Creates an autoload form
/// `(autoload FILE DOCSTRING INTERACTIVE TYPE)` and stores it as the function
/// cell of the symbol.  Also registers an [`AutoloadEntry`] with the
/// evaluator's [`AutoloadManager`].  Returns the function name symbol.
pub(crate) fn sf_autoload(
    eval: &mut super::eval::Evaluator,
    tail: &[super::expr::Expr],
) -> super::error::EvalResult {
    let mut args = Vec::with_capacity(tail.len());
    for expr in tail {
        args.push(eval.eval(expr)?);
    }
    register_autoload_in_state(&mut eval.obarray, &mut eval.autoloads, &args)
}

/// `(eval-when-compile &rest BODY)`
///
/// In the interpreter, evaluates BODY sequentially and returns the last
/// result.  This mirrors GNU Emacs's interpreter semantics: the macro
/// expansion of `eval-when-compile` in `byte-run.el` ultimately computes
/// the body via `eval` and exposes the result as a quoted constant.
///
/// Swallowing errors here is wrong because it hides real bootstrap
/// dependency/order bugs.  Source bootstrap relies on forms like
/// `(eval-when-compile (require 'cl-lib))` actually running; if they fail,
/// the failure must surface so bootstrap can be fixed rather than masked.
pub(crate) fn sf_eval_when_compile(
    eval: &mut super::eval::Evaluator,
    tail: &[super::expr::Expr],
) -> super::error::EvalResult {
    // In GNU Emacs, eval-when-compile evaluates BODY during byte-compilation
    // and replaces the form with the result constant in the .elc file.
    // When loading .elc, the body was already evaluated and the result is
    // inlined as a constant — the body does NOT run at load time.
    //
    // NeoVM loads .el source, so it evaluates the body (same as progn).
    // BUT: .el files like eieio-core.el use eval-when-compile to set
    // compile-time-only state (e.g., safety=0) that should NOT affect
    // runtime code generation. In GNU Emacs, these files ship as .elc
    // where eval-when-compile was already folded to a constant.
    //
    // To match .elc semantics: when loading a file, return nil instead
    // of evaluating the body. This prevents compile-time side effects
    // (like cl-declaim safety settings) from leaking into runtime
    // struct accessor generation.
    // NOTE: In GNU Emacs, eval-when-compile is a MACRO defined in
    // byte-run.el that evaluates its body during macro expansion and
    // returns (quote RESULT). The Rust special form handler here is
    // only reached if the Elisp macro is not yet loaded (early bootstrap).
    // Once byte-run.el loads, the macro takes priority and this code
    // is never called.
    eval.sf_progn(tail)
}

/// `(eval-and-compile &rest BODY)`
///
/// In the interpreter, simply evaluates BODY sequentially and returns the last
/// result (identical to `progn`).
pub(crate) fn sf_eval_and_compile(
    eval: &mut super::eval::Evaluator,
    tail: &[super::expr::Expr],
) -> super::error::EvalResult {
    eval.sf_progn(tail)
}

// ---------------------------------------------------------------------------
// GcTrace
// ---------------------------------------------------------------------------

impl GcTrace for AutoloadManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for values in self.after_load.values() {
            for value in values {
                roots.push(*value);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "autoload_test.rs"]
mod tests;
