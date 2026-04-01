//! Debugger and help system.
//!
//! Implements:
//! - Backtrace frames and stack introspection
//! - describe-function, describe-variable
//! - Debug-on-entry and breakpoint state
//! - Breakpoints and stepping
//! - Apropos searching
//! - Doc string storage and retrieval

use std::collections::{HashMap, HashSet};

use super::intern::resolve_sym;
use super::print::print_value;
use super::value::{Value, ValueKind, VecLikeType};

// ---------------------------------------------------------------------------
// Argument validation helpers (local to this module)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Backtrace
// ---------------------------------------------------------------------------

/// A single stack frame in a backtrace.
#[derive(Clone, Debug)]
pub struct BacktraceFrame {
    /// Name of the function being called.
    pub function: String,
    /// Arguments passed to the function.
    pub args: Vec<Value>,
    /// Source file (if known).
    pub file: Option<String>,
    /// Source line (if known).
    pub line: Option<usize>,
    /// Whether this is a special form (e.g. `if`, `let`).
    pub is_special_form: bool,
}

/// A collection of backtrace frames representing the call stack.
#[derive(Clone, Debug)]
pub struct Backtrace {
    frames: Vec<BacktraceFrame>,
    max_depth: usize,
}

impl Default for Backtrace {
    fn default() -> Self {
        Self::new()
    }
}

impl Backtrace {
    /// Create a new empty backtrace with the default max depth.
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            max_depth: 100,
        }
    }

    /// Create a backtrace with a custom max depth.
    pub fn with_max_depth(max_depth: usize) -> Self {
        Self {
            frames: Vec::new(),
            max_depth,
        }
    }

    /// Push a frame onto the backtrace.  Silently drops frames beyond max depth.
    pub fn push(&mut self, frame: BacktraceFrame) {
        if self.frames.len() < self.max_depth {
            self.frames.push(frame);
        }
    }

    /// Pop the most recent frame.
    pub fn pop(&mut self) -> Option<BacktraceFrame> {
        self.frames.pop()
    }

    /// Current depth (number of frames).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Access the frames slice.
    pub fn frames(&self) -> &[BacktraceFrame] {
        &self.frames
    }

    /// Format the backtrace as a human-readable string, most recent frame first.
    pub fn format(&self) -> String {
        if self.frames.is_empty() {
            return "  (no backtrace)\n".to_string();
        }
        let mut out = String::new();
        // Print newest frame first (like Emacs *Backtrace* buffer)
        for (i, frame) in self.frames.iter().rev().enumerate() {
            let kind = if frame.is_special_form { "  " } else { "  " };
            let args_str = frame
                .args
                .iter()
                .map(print_value)
                .collect::<Vec<_>>()
                .join(" ");
            let loc = match (&frame.file, frame.line) {
                (Some(f), Some(l)) => format!(" [{}:{}]", f, l),
                (Some(f), None) => format!(" [{}]", f),
                _ => String::new(),
            };
            let marker = if frame.is_special_form { "*" } else { "" };
            out.push_str(&format!(
                "{}{}{}({}{}){}\n",
                kind,
                marker,
                if marker.is_empty() { "" } else { " " },
                frame.function,
                if args_str.is_empty() {
                    String::new()
                } else {
                    format!(" {}", args_str)
                },
                loc,
            ));
            // Guard against huge backtraces in display
            if i >= 99 {
                out.push_str("  ...(truncated)\n");
                break;
            }
        }
        out
    }

    /// Remove all frames.
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

// ---------------------------------------------------------------------------
// DebugAction
// ---------------------------------------------------------------------------

/// What the debugger should do when triggered.
#[derive(Clone, Debug)]
pub enum DebugAction {
    /// Continue execution normally.
    Continue,
    /// Step into the next form.
    Step,
    /// Step over the next form (evaluate it, then stop).
    Next,
    /// Finish the current function and stop at caller.
    Finish,
    /// Abort evaluation entirely.
    Quit,
    /// Evaluate an expression string in the current context.
    Eval(String),
}

// ---------------------------------------------------------------------------
// Breakpoint
// ---------------------------------------------------------------------------

/// A breakpoint set on a function.
#[derive(Clone, Debug)]
pub struct Breakpoint {
    /// Unique identifier.
    pub id: usize,
    /// The function this breakpoint is set on.
    pub function: String,
    /// Whether the breakpoint is currently enabled.
    pub enabled: bool,
    /// Optional condition expression (source string).
    pub condition: Option<String>,
    /// Number of times this breakpoint has been hit.
    pub hit_count: usize,
}

// ---------------------------------------------------------------------------
// DebugState
// ---------------------------------------------------------------------------

/// Central debug/introspection state for the evaluator.
pub struct DebugState {
    /// Whether the debugger is currently active (stopped at a breakpoint/error).
    pub active: bool,
    /// Set of function names that should trigger the debugger on entry.
    pub debug_on_entry: HashSet<String>,
    /// Whether we are in single-step mode.
    pub stepping: bool,
    /// The current backtrace (populated during evaluation).
    pub current_backtrace: Backtrace,
    /// All breakpoints.
    pub breakpoints: Vec<Breakpoint>,
    /// Next breakpoint id.
    next_bp_id: usize,
}

impl Default for DebugState {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugState {
    /// Create a new debugger session state with entry/breakpoint tracking.
    ///
    /// Error/quit signal policy lives in the shared condition dispatcher in
    /// `eval.rs`, mirroring GNU Emacs's `eval.c` split.
    pub fn new() -> Self {
        Self {
            active: false,
            debug_on_entry: HashSet::new(),
            stepping: false,
            current_backtrace: Backtrace::new(),
            breakpoints: Vec::new(),
            next_bp_id: 1,
        }
    }

    /// Check whether the debugger should be entered when `function` is called.
    pub fn should_debug_on_entry(&self, function: &str) -> bool {
        if self.debug_on_entry.contains(function) {
            return true;
        }
        // Also check breakpoints
        self.breakpoints
            .iter()
            .any(|bp| bp.enabled && bp.function == function)
    }

    /// Mark a function for debug-on-entry.
    pub fn add_debug_on_entry(&mut self, function: &str) {
        self.debug_on_entry.insert(function.to_string());
    }

    /// Remove a function from debug-on-entry.
    pub fn remove_debug_on_entry(&mut self, function: &str) {
        self.debug_on_entry.remove(function);
    }

    /// Add a breakpoint on a function.  Returns the breakpoint id.
    pub fn add_breakpoint(&mut self, function: &str) -> usize {
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        self.breakpoints.push(Breakpoint {
            id,
            function: function.to_string(),
            enabled: true,
            condition: None,
            hit_count: 0,
        });
        id
    }

    /// Add a breakpoint with a condition expression.  Returns the breakpoint id.
    pub fn add_conditional_breakpoint(&mut self, function: &str, condition: &str) -> usize {
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        self.breakpoints.push(Breakpoint {
            id,
            function: function.to_string(),
            enabled: true,
            condition: Some(condition.to_string()),
            hit_count: 0,
        });
        id
    }

    /// Remove a breakpoint by id.  Returns true if found and removed.
    pub fn remove_breakpoint(&mut self, id: usize) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints.retain(|bp| bp.id != id);
        self.breakpoints.len() < before
    }

    /// Toggle a breakpoint's enabled state.  Returns true if the breakpoint was found.
    pub fn toggle_breakpoint(&mut self, id: usize) -> bool {
        for bp in &mut self.breakpoints {
            if bp.id == id {
                bp.enabled = !bp.enabled;
                return true;
            }
        }
        false
    }

    /// Record a breakpoint hit (increment hit_count).
    pub fn record_breakpoint_hit(&mut self, function: &str) {
        for bp in &mut self.breakpoints {
            if bp.enabled && bp.function == function {
                bp.hit_count += 1;
            }
        }
    }

    /// List all breakpoints.
    pub fn list_breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }
}

// ---------------------------------------------------------------------------
// DocStore
// ---------------------------------------------------------------------------

/// Storage for documentation strings (function and variable docs).
pub struct DocStore {
    function_docs: HashMap<String, String>,
    variable_docs: HashMap<String, String>,
}

impl Default for DocStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DocStore {
    /// Create a new empty doc store.
    pub fn new() -> Self {
        Self {
            function_docs: HashMap::new(),
            variable_docs: HashMap::new(),
        }
    }

    /// Set the documentation string for a function.
    pub fn set_function_doc(&mut self, name: &str, doc: &str) {
        self.function_docs.insert(name.to_string(), doc.to_string());
    }

    /// Set the documentation string for a variable.
    pub fn set_variable_doc(&mut self, name: &str, doc: &str) {
        self.variable_docs.insert(name.to_string(), doc.to_string());
    }

    /// Get the documentation string for a function.
    pub fn get_function_doc(&self, name: &str) -> Option<&str> {
        self.function_docs.get(name).map(|s| s.as_str())
    }

    /// Get the documentation string for a variable.
    pub fn get_variable_doc(&self, name: &str) -> Option<&str> {
        self.variable_docs.get(name).map(|s| s.as_str())
    }

    /// Search for symbols whose names contain `pattern` (case-insensitive substring).
    /// Returns a vec of (name, has_function_doc, has_variable_doc).
    pub fn apropos(&self, pattern: &str) -> Vec<(String, bool, bool)> {
        let pattern_lower = pattern.to_lowercase();
        let mut seen: HashMap<String, (bool, bool)> = HashMap::new();

        for name in self.function_docs.keys() {
            if name.to_lowercase().contains(&pattern_lower) {
                let entry = seen.entry(name.clone()).or_insert((false, false));
                entry.0 = true;
            }
        }
        for name in self.variable_docs.keys() {
            if name.to_lowercase().contains(&pattern_lower) {
                let entry = seen.entry(name.clone()).or_insert((false, false));
                entry.1 = true;
            }
        }

        let mut results: Vec<(String, bool, bool)> = seen
            .into_iter()
            .map(|(name, (has_func, has_var))| (name, has_func, has_var))
            .collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        results
    }

    /// Return all function names that have documentation, sorted.
    pub fn all_documented_functions(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.function_docs.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Return all variable names that have documentation, sorted.
    pub fn all_documented_variables(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.variable_docs.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Remove documentation for a function.
    pub fn remove_function_doc(&mut self, name: &str) -> bool {
        self.function_docs.remove(name).is_some()
    }

    /// Remove documentation for a variable.
    pub fn remove_variable_doc(&mut self, name: &str) -> bool {
        self.variable_docs.remove(name).is_some()
    }
}

// ---------------------------------------------------------------------------
// HelpFormatter
// ---------------------------------------------------------------------------

/// Formats help buffer content (describe-function, describe-variable, etc.).
pub struct HelpFormatter;

impl HelpFormatter {
    /// Format a `describe-function` help string.
    pub fn describe_function(name: &str, value: &Value, doc: Option<&str>) -> String {
        let mut out = String::new();

        let kind = match value.kind() {
            ValueKind::Veclike(VecLikeType::Lambda) => {
                if let Some(lam) = value.get_lambda_data() {
                    if lam.env.is_some() {
                        "a Lisp closure"
                    } else {
                        "a Lisp function"
                    }
                } else {
                    "a Lisp function"
                }
            }
            ValueKind::Subr(_) => "a built-in function",
            ValueKind::Veclike(VecLikeType::Macro) => "a Lisp macro",
            ValueKind::Veclike(VecLikeType::ByteCode) => "a compiled Lisp function",
            _ => "a Lisp function",
        };

        out.push_str(&format!("{} is {}.\n\n", name, kind));

        // Signature
        match value.kind() {
            ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
                if let Some(lam) = value.get_lambda_data() {
                    let params = format_param_list(&lam.params);
                    out.push_str(&format!("({}{})\n", name, params));
                }
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                if let Some(bc) = value.get_bytecode_data() {
                    let params = format_param_list(&bc.params);
                    out.push_str(&format!("({}{})\n", name, params));
                }
            }
            ValueKind::Subr(id) => {
                out.push_str(&format!("({} &rest ARGS)\n", resolve_sym(id)));
            }
            _ => {
                out.push_str(&format!("({})\n", name));
            }
        }

        // Docstring from LambdaData
        let inline_doc = match value.kind() {
            ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
                value
                    .get_lambda_data()
                    .and_then(|lam| lam.docstring.as_deref())
            }
            _ => None,
        };

        // Prefer the docstore doc, fall back to inline
        let doc_text = doc.or(inline_doc);

        if let Some(d) = doc_text {
            out.push('\n');
            out.push_str(d);
            if !d.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str("\nNot documented.\n");
        }

        out
    }

    /// Format a `describe-variable` help string.
    pub fn describe_variable(name: &str, value: &Value, doc: Option<&str>) -> String {
        let mut out = String::new();

        let printed = print_value(value);
        out.push_str(&format!("{}'s value is {}\n", name, printed));

        if let Some(d) = doc {
            out.push_str("\nDocumentation:\n");
            out.push_str(d);
            if !d.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str("\nNot documented.\n");
        }

        out
    }

    /// Format a `describe-key` help string.
    pub fn describe_key(key: &str, binding: &str, doc: Option<&str>) -> String {
        let mut out = String::new();

        out.push_str(&format!("{} runs the command {}\n", key, binding));

        if let Some(d) = doc {
            out.push('\n');
            out.push_str(d);
            if !d.ends_with('\n') {
                out.push('\n');
            }
        }

        out
    }

    /// Format an apropos result listing.
    pub fn format_apropos(entries: &[(String, bool, bool)]) -> String {
        if entries.is_empty() {
            return "No matches.\n".to_string();
        }
        let mut out = String::new();
        for (name, has_func, has_var) in entries {
            let mut kinds = Vec::new();
            if *has_func {
                kinds.push("Function");
            }
            if *has_var {
                kinds.push("Variable");
            }
            out.push_str(&format!("{}\n  {}\n", name, kinds.join(", ")));
        }
        out
    }
}

/// Format a parameter list for display in help output.
fn format_param_list(params: &super::value::LambdaParams) -> String {
    let mut parts = Vec::new();
    for p in &params.required {
        parts.push(resolve_sym(*p).to_uppercase());
    }
    if !params.optional.is_empty() {
        parts.push("&optional".to_string());
        for p in &params.optional {
            parts.push(resolve_sym(*p).to_uppercase());
        }
    }
    if let Some(rest) = params.rest {
        parts.push("&rest".to_string());
        parts.push(resolve_sym(rest).to_uppercase());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Built-in helper function
// ---------------------------------------------------------------------------

// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "debug_test.rs"]
mod tests;
