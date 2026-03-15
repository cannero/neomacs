//! Interactive command system and mode definition macros.
//!
//! Implements:
//! - `InteractiveSpec` and `InteractiveRegistry` for tracking which functions
//!   are interactive commands and their argument specifications.
//! - Built-in functions: `call-interactively`, `interactive-p`,
//!   `called-interactively-p`, `commandp`,
//!   `key-binding`, `local-key-binding`,
//!   `minor-mode-key-binding`, `where-is-internal`,
//!   `describe-key-briefly`, `this-command-keys`,
//!   `this-command-keys-vector`, `thing-at-point`, `bounds-of-thing-at-point`,
//!   `symbol-at-point`.
//! - Special forms: `define-minor-mode`, `define-derived-mode`,
//!   `define-generic-mode`.

use std::collections::{HashMap, HashSet};

use super::error::{EvalResult, Flow, signal};
use super::eval::{Evaluator, quote_to_value, value_to_expr};
use super::expr::Expr;
use super::intern::{intern, resolve_sym};
use super::keymap::{
    KeyEvent, format_key_event, format_key_sequence, is_list_keymap, key_event_to_emacs_event,
    list_keymap_for_each_binding, list_keymap_lookup_one, list_keymap_lookup_seq, make_list_keymap,
    make_sparse_list_keymap,
};
use super::mode::{MajorMode, MinorMode};
use super::value::*;

// ---------------------------------------------------------------------------
// InteractiveSpec — describes how a command reads its arguments
// ---------------------------------------------------------------------------

/// Interactive argument specification for a command.
#[derive(Clone, Debug)]
pub struct InteractiveSpec {
    /// Code letter(s) describing argument types, e.g. "r" for region,
    /// "p" for prefix arg, "sPrompt: " for string prompt, etc.
    pub code: String,
    /// Optional prompt string (extracted from the code).
    pub prompt: Option<String>,
}

impl InteractiveSpec {
    /// Create a new interactive spec from a code string.
    pub fn new(code: impl Into<String>) -> Self {
        let code = code.into();
        // Extract prompt from code if it contains a prompt (e.g. "sEnter name: ")
        let prompt = if code.len() > 1 && code.starts_with(|c: char| c.is_ascii_lowercase()) {
            Some(code[1..].to_string())
        } else {
            None
        };
        Self { code, prompt }
    }

    /// Create a spec with no arguments (plain interactive command).
    pub fn no_args() -> Self {
        Self {
            code: String::new(),
            prompt: None,
        }
    }
}

// ---------------------------------------------------------------------------
// InteractiveRegistry — tracks which functions are interactive commands
// ---------------------------------------------------------------------------

/// Registry for interactive command specifications.
///
/// Tracks which named functions are interactive (i.e., can be called via
/// `M-x` or key bindings) and their argument specs.
pub struct InteractiveRegistry {
    /// Map from function name to its interactive spec.
    specs: HashMap<String, InteractiveSpec>,
    /// Stack tracking whether the current function was called interactively.
    interactive_call_stack: Vec<bool>,
    /// The key sequence that invoked the current command (if any).
    this_command_keys: Vec<String>,
}

impl InteractiveRegistry {
    pub fn new() -> Self {
        Self {
            specs: HashMap::new(),
            interactive_call_stack: Vec::new(),
            this_command_keys: Vec::new(),
        }
    }

    /// Register a function as interactive with the given spec.
    pub fn register_interactive(&mut self, name: &str, spec: InteractiveSpec) {
        self.specs.insert(name.to_string(), spec);
    }

    /// Check if a function is registered as interactive.
    pub fn is_interactive(&self, name: &str) -> bool {
        self.specs.contains_key(name)
    }

    /// Get the interactive spec for a function, if registered.
    pub fn get_spec(&self, name: &str) -> Option<&InteractiveSpec> {
        self.specs.get(name)
    }

    /// Push an interactive call frame.
    pub fn push_interactive_call(&mut self, is_interactive: bool) {
        self.interactive_call_stack.push(is_interactive);
    }

    /// Pop an interactive call frame.
    pub fn pop_interactive_call(&mut self) {
        self.interactive_call_stack.pop();
    }

    /// Check if the current function was called interactively.
    pub fn is_called_interactively(&self) -> bool {
        self.interactive_call_stack.last().copied().unwrap_or(false)
    }

    /// Set the key sequence that invoked the current command.
    pub fn set_this_command_keys(&mut self, keys: Vec<String>) {
        self.this_command_keys = keys;
    }

    /// Get the key sequence that invoked the current command.
    pub fn this_command_keys(&self) -> &[String] {
        &self.this_command_keys
    }

    // pdump accessors
    pub(crate) fn dump_specs(&self) -> &HashMap<String, InteractiveSpec> {
        &self.specs
    }
    pub(crate) fn from_dump(specs: HashMap<String, InteractiveSpec>) -> Self {
        Self {
            specs,
            interactive_call_stack: Vec::new(),
            this_command_keys: Vec::new(),
        }
    }
}

impl Default for InteractiveRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Expect helpers (local to this module)
// ---------------------------------------------------------------------------

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

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_optional_command_keys_vector(keys: Option<&Value>) -> Result<(), Flow> {
    if let Some(keys_value) = keys {
        if !keys_value.is_nil() && !keys_value.is_vector() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("vectorp"), *keys_value],
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Built-in functions (evaluator-dependent)
// ---------------------------------------------------------------------------

/// `(call-interactively FUNCTION &optional RECORD-FLAG KEYS)`
/// Call FUNCTION interactively, reading arguments according to its
/// interactive spec.
pub(crate) fn builtin_call_interactively(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("call-interactively", &args, 1)?;
    expect_max_args("call-interactively", &args, 3)?;
    expect_optional_command_keys_vector(args.get(2))?;

    let func_val = &args[0];
    if !command_designator_p(eval, func_val) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("commandp"), *func_val],
        ));
    }
    let Some((resolved_name, func)) = resolve_command_target(eval, func_val) else {
        return Err(signal("void-function", vec![*func_val]));
    };
    let func = normalize_command_callable(eval, func)?;
    let mut context = InteractiveInvocationContext::from_keys_arg(eval, args.get(2));
    let call_args = resolve_interactive_invocation_args(
        eval,
        &resolved_name,
        &func,
        CommandInvocationKind::CallInteractively,
        &mut context,
    )?;

    // Mark as interactive call
    eval.interactive.push_interactive_call(true);

    let result = eval.apply(func, call_args);

    eval.interactive.pop_interactive_call();
    result
}

/// `(interactive-p)` -> t if the calling function was called interactively.
pub(crate) fn builtin_interactive_p(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("interactive-p", &args, 0)?;
    let _ = eval;
    // Emacs 30 keeps `interactive-p` obsolete; it effectively returns nil.
    Ok(Value::Nil)
}

/// `(called-interactively-p &optional KIND)`
/// Return t if the calling function was called interactively.
/// KIND can be 'interactive or 'any.
pub(crate) fn builtin_called_interactively_p(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    // Accept 0 or 1 args
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("called-interactively-p"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    if !eval.interactive.is_called_interactively() {
        return Ok(Value::Nil);
    }

    // GNU Emacs semantics:
    // - KIND = 'interactive => nil
    // - KIND = nil / 'any / unknown => t (when called interactively)
    if args
        .first()
        .is_some_and(|v| matches!(v, Value::Symbol(id) if resolve_sym(*id) == "interactive"))
    {
        Ok(Value::Nil)
    } else {
        Ok(Value::True)
    }
}

/// `(commandp FUNCTION &optional FOR-CALL-INTERACTIVELY)`
/// Return non-nil if FUNCTION is a command (i.e., can be called interactively).
pub(crate) fn builtin_commandp_interactive(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("commandp", &args, 1)?;
    expect_max_args("commandp", &args, 2)?;
    let is_command = command_designator_p(eval, &args[0]);
    Ok(Value::bool(is_command))
}

/// `(command-modes COMMAND)` -- return COMMAND's mode list.
///
/// Current compatibility behavior returns nil.
pub(crate) fn builtin_command_modes(args: Vec<Value>) -> EvalResult {
    expect_args("command-modes", &args, 1)?;
    Ok(Value::Nil)
}

/// `(command-remapping COMMAND &optional POSITION KEYMAP)` -- return remapped
/// command for COMMAND.
///
/// Respects local/global keymaps when KEYMAP is omitted or nil.
pub(crate) fn builtin_command_remapping(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("command-remapping", &args, 1)?;
    expect_max_args("command-remapping", &args, 3)?;
    if let Some(keymap) = args.get(2) {
        if !keymap.is_nil() && !command_remapping_keymap_arg_valid(eval, keymap) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("keymapp"), *keymap],
            ));
        }
    }
    let Some(command_name) = command_remapping_command_name(&args[0]) else {
        return Ok(Value::Nil);
    };
    if let Some(position) = args.get(1) {
        interactive_validate_integer_position_arg(eval, position)?;
    }
    if let Some(keymap_arg) = args.get(2) {
        match keymap_arg {
            Value::Cons(keymap) => {
                let keymap_value = Value::Cons(*keymap);
                if let Some(target) =
                    command_remapping_lookup_in_lisp_keymap(&keymap_value, &command_name)
                {
                    return Ok(command_remapping_normalize_target(target));
                }
                return Ok(Value::Nil);
            }
            Value::Nil => {
                return Ok(
                    command_remapping_lookup_in_active_keymaps(eval, &command_name)
                        .unwrap_or(Value::Nil),
                );
            }
            _ => {
                // Not a valid keymap
                return Ok(Value::Nil);
            }
        }
    }
    Ok(command_remapping_lookup_in_active_keymaps(eval, &command_name).unwrap_or(Value::Nil))
}

fn builtin_command_name(name: &str) -> bool {
    matches!(
        name,
        "ignore"
            | "self-insert-command"
            | "newline"
            | "forward-char"
            | "backward-char"
            | "delete-char"
            | "insert-char"
            | "kill-line"
            | "kill-word"
            | "backward-kill-word"
            | "kill-region"
            | "kill-ring-save"
            | "kill-whole-line"
            | "copy-region-as-kill"
            | "yank"
            | "yank-pop"
            | "transpose-chars"
            | "transpose-lines"
            | "transpose-paragraphs"
            | "transpose-sentences"
            | "transpose-sexps"
            | "transpose-words"
            | "open-line"
            | "delete-horizontal-space"
            | "just-one-space"
            | "delete-indentation"
            | "upcase-word"
            | "downcase-word"
            | "capitalize-word"
            | "upcase-region"
            | "downcase-region"
            | "capitalize-region"
            | "upcase-initials-region"
            | "switch-to-buffer"
            | "select-frame"
            | "recenter-top-bottom"
            | "scroll-up-command"
            | "scroll-down-command"
            | "other-window"
            | "keyboard-quit"
            | "beginning-of-line"
            | "end-of-line"
            | "move-beginning-of-line"
            | "move-end-of-line"
            | "abort-minibuffers"
            | "abort-recursive-edit"
            | "add-name-to-file"
            | "advice-remove"
            | "backward-sexp"
            | "backward-word"
            | "base64-decode-region"
            | "base64-encode-region"
            | "base64url-encode-region"
            | "buffer-disable-undo"
            | "buffer-enable-undo"
            | "call-last-kbd-macro"
            | "copy-file"
            | "copy-to-register"
            | "defining-kbd-macro"
            | "delete-directory"
            | "delete-file"
            | "delete-frame"
            | "delete-other-windows"
            | "delete-other-windows-internal"
            | "delete-process"
            | "kill-process"
            | "signal-process"
            | "delete-region"
            | "delete-window"
            | "decode-coding-region"
            | "do-auto-save"
            | "handle-save-session"
            | "handle-switch-frame"
            | "display-buffer"
            | "emacs-version"
            | "end-kbd-macro"
            | "erase-buffer"
            | "eval-buffer"
            | "eval-region"
            | "encode-coding-region"
            | "exit-minibuffer"
            | "exit-recursive-edit"
            | "expand-abbrev"
            | "fit-window-to-buffer"
            | "flush-lines"
            | "forward-line"
            | "forward-sexp"
            | "forward-word"
            | "garbage-collect"
            | "getenv"
            | "global-set-key"
            | "gui-set-selection"
            | "goto-char"
            | "how-many"
            | "increment-register"
            | "indent-rigidly"
            | "indent-to"
            | "insert-register"
            | "iconify-frame"
            | "isearch-backward"
            | "isearch-forward"
            | "keep-lines"
            | "kill-buffer"
            | "kill-emacs"
            | "kill-local-variable"
            | "load-file"
            | "lower-frame"
            | "lossage-size"
            | "malloc-info"
            | "malloc-trim"
            | "local-set-key"
            | "make-directory"
            | "make-frame"
            | "make-frame-invisible"
            | "make-frame-visible"
            | "make-indirect-buffer"
            | "make-local-variable"
            | "make-symbolic-link"
            | "make-variable-buffer-local"
            | "modify-syntax-entry"
            | "move-to-column"
            | "move-to-window-line"
            | "narrow-to-region"
            | "newline-and-indent"
            | "number-to-register"
            | "open-dribble-file"
            | "open-termscript"
            | "point-to-register"
            | "pop-to-buffer"
            | "posix-search-backward"
            | "posix-search-forward"
            | "query-replace-regexp"
            | "raise-frame"
            | "recenter"
            | "redirect-debugging-output"
            | "re-search-backward"
            | "re-search-forward"
            | "recursive-edit"
            | "rename-buffer"
            | "rename-file"
            | "replace-buffer-contents"
            | "replace-regexp"
            | "replace-string"
            | "replace-rectangle"
            | "redraw-display"
            | "run-at-time"
            | "run-with-idle-timer"
            | "run-with-timer"
            | "scroll-down"
            | "scroll-left"
            | "scroll-right"
            | "search-backward"
            | "search-forward"
            | "scroll-up"
            | "set-file-modes"
            | "set-frame-height"
            | "set-frame-width"
            | "set-buffer-process-coding-system"
            | "set-keyboard-coding-system"
            | "set-terminal-coding-system"
            | "setenv"
            | "start-kbd-macro"
            | "tab-to-tab-stop"
            | "suspend-emacs"
            | "top-level"
            | "transient-mark-mode"
            | "transpose-regions"
            | "undo"
            | "unix-sync"
            | "view-register"
            | "widen"
            | "word-search-backward"
            | "word-search-forward"
            | "write-region"
            | "x-menu-bar-open-internal"
    )
}

fn expr_is_interactive_form(expr: &Expr) -> bool {
    match expr {
        Expr::List(items) => items.first().is_some_and(
            |head| matches!(head, Expr::Symbol(id) if resolve_sym(*id) == "interactive"),
        ),
        _ => false,
    }
}

fn expr_is_declare_form(expr: &Expr) -> bool {
    match expr {
        Expr::List(items) => items
            .first()
            .is_some_and(|head| matches!(head, Expr::Symbol(id) if resolve_sym(*id) == "declare")),
        _ => false,
    }
}

fn lambda_body_metadata_end(body: &[Expr]) -> usize {
    let mut body_index = 0;
    if matches!(body.first(), Some(Expr::Str(_))) {
        body_index = 1;
    }
    while body.get(body_index).is_some_and(expr_is_declare_form) {
        body_index += 1;
    }
    body_index
}

fn lambda_body_has_interactive_form(body: &[Expr]) -> bool {
    let body_index = lambda_body_metadata_end(body);
    body.get(body_index).is_some_and(expr_is_interactive_form)
}

fn value_list_to_vec(list: &Value) -> Option<Vec<Value>> {
    let mut values = Vec::new();
    let mut cursor = *list;
    loop {
        match cursor {
            Value::Nil => return Some(values),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                values.push(pair.car);
                cursor = pair.cdr;
            }
            _ => return None,
        }
    }
}

fn value_is_interactive_form(value: &Value) -> bool {
    match value {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            pair.car.as_symbol_name() == Some("interactive")
        }
        _ => false,
    }
}

fn value_is_interactive_autoload(value: &Value) -> bool {
    if !super::autoload::is_autoload_value(value) {
        return false;
    }
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    !matches!(items.get(3), None | Some(Value::Nil))
}

fn quoted_lambda_has_interactive_form(value: &Value) -> bool {
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    if items.first().and_then(Value::as_symbol_name) != Some("lambda") {
        return false;
    }

    let mut body_index = 2;
    if matches!(items.get(body_index), Some(Value::Str(_))) {
        body_index += 1;
    }
    while items.get(body_index).is_some_and(value_is_declare_form) {
        body_index += 1;
    }

    items.get(body_index).is_some_and(value_is_interactive_form)
}

fn value_is_declare_form(value: &Value) -> bool {
    match value {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            pair.car.as_symbol_name() == Some("declare")
        }
        _ => false,
    }
}

fn resolve_function_designator_symbol(eval: &Evaluator, name: &str) -> Option<(String, Value)> {
    let mut current = name.to_string();
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current.clone()) {
            return None;
        }

        if eval.obarray.is_function_unbound(&current) {
            return None;
        }

        if let Some(function) = eval.obarray.symbol_function(&current) {
            if let Some(next) = function.as_symbol_name() {
                if next == "nil" {
                    return Some((current, Value::Nil));
                }
                current = next.to_string();
                continue;
            }
            return Some((current, *function));
        }

        if let Some(function) = super::subr_info::fallback_macro_value(&current) {
            return Some((current, function));
        }

        if super::subr_info::is_special_form(&current)
            || super::subr_info::is_evaluator_callable_name(&current)
            || super::builtin_registry::is_dispatch_builtin_name(&current)
        {
            return Some((current.clone(), Value::Subr(intern(&current))));
        }

        return None;
    }
}

fn command_object_p(eval: &Evaluator, resolved_name: Option<&str>, value: &Value) -> bool {
    if let Some(name) = resolved_name {
        if eval.interactive.is_interactive(name) || builtin_command_name(name) {
            return true;
        }
    }
    if value_is_interactive_autoload(value) {
        return true;
    }

    match value {
        Value::Lambda(_) => {
            if let Some(lambda) = value.get_lambda_data() {
                lambda_body_has_interactive_form(&lambda.body)
            } else {
                false
            }
        }
        Value::Cons(_) => quoted_lambda_has_interactive_form(value),
        Value::Subr(id) => {
            let name = resolve_sym(*id);
            eval.interactive.is_interactive(name) || builtin_command_name(name)
        }
        _ => false,
    }
}

fn command_designator_p(eval: &Evaluator, designator: &Value) -> bool {
    if let Some(name) = designator.as_symbol_name() {
        if eval.obarray.is_function_unbound(name) {
            return false;
        }
        if let Some((resolved_name, resolved_value)) =
            resolve_function_designator_symbol(eval, name)
        {
            return command_object_p(eval, Some(&resolved_name), &resolved_value);
        }
        return eval.interactive.is_interactive(name) || builtin_command_name(name);
    }
    command_object_p(eval, None, designator)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandInvocationKind {
    CallInteractively,
    CommandExecute,
}

#[derive(Clone, Debug)]
enum ParsedInteractiveSpec {
    NoArgs,
    StringCode(String),
    Form(Expr),
}

#[derive(Clone, Debug, Default)]
struct ParsedInteractiveStringCode {
    prefix_flags: Vec<char>,
    entries: Vec<(char, String)>,
}

#[derive(Clone, Debug, Default)]
struct InteractiveInvocationContext {
    command_keys: Vec<Value>,
    next_event_with_parameters_index: usize,
    has_command_keys_context: bool,
}

impl InteractiveInvocationContext {
    fn from_keys_arg(eval: &Evaluator, keys: Option<&Value>) -> Self {
        let mut context = Self::default();
        if let Some(Value::Vector(values)) = keys {
            let values = with_heap(|h| h.get_vector(*values).clone());
            if !values.is_empty() {
                context.command_keys = values.clone();
                context.has_command_keys_context = true;
                return context;
            }
        }
        if !eval.read_command_keys().is_empty() {
            context.command_keys = eval.read_command_keys().to_vec();
            context.has_command_keys_context = true;
        }
        context
    }
}

fn dynamic_or_global_symbol_value(eval: &Evaluator, name: &str) -> Option<Value> {
    let name_id = intern(name);
    for frame in eval.dynamic.iter().rev() {
        if let Some(v) = frame.get(&name_id) {
            return Some(*v);
        }
    }
    eval.obarray.symbol_value(name).cloned()
}

fn dynamic_buffer_or_global_symbol_value(
    eval: &Evaluator,
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in eval.dynamic.iter().rev() {
        if let Some(v) = frame.get(&name_id) {
            return Some(*v);
        }
    }
    if let Some(v) = buf.get_buffer_local(name) {
        return Some(*v);
    }
    eval.obarray.symbol_value(name).cloned()
}

fn prefix_numeric_value(value: &Value) -> i64 {
    match value {
        Value::Nil => 1,
        Value::Int(n) => *n,
        Value::Float(f, _) => *f as i64,
        Value::Char(c) => *c as i64,
        Value::Symbol(id) if resolve_sym(*id) == "-" => -1,
        Value::Cons(cell) => {
            let car = {
                let pair = read_cons(*cell);
                pair.car
            };
            match car {
                Value::Int(n) => n,
                Value::Float(f, _) => f as i64,
                Value::Char(c) => c as i64,
                _ => 1,
            }
        }
        _ => 1,
    }
}

fn interactive_prefix_raw_arg(eval: &Evaluator, kind: CommandInvocationKind) -> Value {
    let symbol = match kind {
        CommandInvocationKind::CallInteractively => "current-prefix-arg",
        CommandInvocationKind::CommandExecute => "prefix-arg",
    };
    dynamic_or_global_symbol_value(eval, symbol).unwrap_or(Value::Nil)
}

fn interactive_prefix_numeric_arg(eval: &Evaluator, kind: CommandInvocationKind) -> Value {
    let raw = interactive_prefix_raw_arg(eval, kind);
    Value::Int(prefix_numeric_value(&raw))
}

fn interactive_region_args(
    eval: &Evaluator,
    missing_mark_signal: &str,
) -> Result<Vec<Value>, Flow> {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let mark = buf.mark().ok_or_else(|| {
        signal(
            missing_mark_signal,
            vec![Value::string(
                "The mark is not set now, so there is no region",
            )],
        )
    })?;
    let pt = buf.point();
    let beg = pt.min(mark);
    let end = pt.max(mark);
    // Region-taking builtins use Emacs-style 1-based character positions.
    let beg_char = buf.text.byte_to_char(beg) as i64 + 1;
    let end_char = buf.text.byte_to_char(end) as i64 + 1;
    Ok(vec![Value::Int(beg_char), Value::Int(end_char)])
}

fn interactive_point_arg(eval: &Evaluator) -> Result<Value, Flow> {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_char = buf.point_char() as i64 + 1;
    Ok(Value::Int(point_char))
}

fn interactive_mark_arg(eval: &Evaluator) -> Result<Value, Flow> {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.mark()
        .ok_or_else(|| signal("error", vec![Value::string("The mark is not set now")]))?;
    let mark_char = buf.mark_char().expect("mark byte/char stay in sync") as i64 + 1;
    Ok(Value::Int(mark_char))
}

fn interactive_read_expression_arg(eval: &mut Evaluator, prompt: String) -> Result<Value, Flow> {
    let input = super::reader::builtin_read_from_minibuffer(eval, vec![Value::string(prompt)])?;
    super::reader::builtin_read(eval, vec![input])
}

fn interactive_read_coding_system_optional_arg(prompt: String) -> Result<Value, Flow> {
    match super::lread::builtin_read_coding_system(vec![Value::string(prompt)]) {
        Ok(value) => Ok(value),
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file" => Ok(Value::Nil),
        Err(flow) => Err(flow),
    }
}

fn interactive_buffer_read_only_active(eval: &Evaluator, buf: &crate::buffer::Buffer) -> bool {
    if buf.read_only {
        return true;
    }
    dynamic_buffer_or_global_symbol_value(eval, buf, "buffer-read-only")
        .is_some_and(|v| v.is_truthy())
}

fn interactive_require_writable_current_buffer(eval: &Evaluator) -> Result<(), Flow> {
    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(());
    };
    if dynamic_buffer_or_global_symbol_value(eval, buf, "inhibit-read-only")
        .is_some_and(|v| v.is_truthy())
    {
        return Ok(());
    }
    if interactive_buffer_read_only_active(eval, buf) {
        return Err(signal("buffer-read-only", vec![Value::string(&buf.name)]));
    }
    Ok(())
}

fn interactive_apply_shift_selection_prefix(eval: &mut Evaluator) {
    let shifted = dynamic_or_global_symbol_value(eval, "this-command-keys-shift-translated")
        .is_some_and(|v| v.is_truthy());
    let shift_select_mode =
        dynamic_or_global_symbol_value(eval, "shift-select-mode").is_some_and(|v| v.is_truthy());
    if !shifted || !shift_select_mode {
        return;
    }

    let mut mark_activated = false;
    if let Some(current_id) = eval.buffers.current_buffer_id() {
        let point = eval
            .buffers
            .get(current_id)
            .map(|buf| buf.point())
            .unwrap_or(0);
        let _ = eval.buffers.set_buffer_mark(current_id, point);
        let _ = eval
            .buffers
            .set_buffer_local_property(current_id, "mark-active", Value::True);
        mark_activated = true;
    }
    if mark_activated {
        eval.assign("mark-active", Value::True);
    }
}

fn interactive_apply_prefix_flags(eval: &mut Evaluator, prefix_flags: &[char]) -> Result<(), Flow> {
    for prefix_flag in prefix_flags {
        match prefix_flag {
            '*' => interactive_require_writable_current_buffer(eval)?,
            '@' => {
                // Selecting the window from the first mouse event requires command-loop
                // event context; current batch paths have no such events yet.
            }
            '^' => interactive_apply_shift_selection_prefix(eval),
            _ => {}
        }
    }
    Ok(())
}

fn interactive_event_with_parameters_p(event: &Value) -> bool {
    matches!(event, Value::Cons(_))
}

fn interactive_next_event_with_parameters_from_keys(
    context: &mut InteractiveInvocationContext,
) -> Option<Value> {
    while context.next_event_with_parameters_index < context.command_keys.len() {
        let event = context.command_keys[context.next_event_with_parameters_index];
        context.next_event_with_parameters_index += 1;
        if interactive_event_with_parameters_p(&event) {
            return Some(event);
        }
    }
    None
}

fn interactive_last_input_event_with_parameters(eval: &Evaluator) -> Option<Value> {
    let event = dynamic_or_global_symbol_value(eval, "last-input-event")?;
    interactive_event_with_parameters_p(&event).then_some(event)
}

fn interactive_next_event_with_parameters(
    eval: &Evaluator,
    context: &mut InteractiveInvocationContext,
) -> Option<Value> {
    if context.has_command_keys_context {
        return interactive_next_event_with_parameters_from_keys(context);
    }
    interactive_last_input_event_with_parameters(eval)
}

fn parse_interactive_spec(expr: &Expr) -> Option<ParsedInteractiveSpec> {
    let Expr::List(items) = expr else {
        return None;
    };
    if !items
        .first()
        .is_some_and(|head| matches!(head, Expr::Symbol(id) if resolve_sym(*id) == "interactive"))
    {
        return None;
    }
    match items.get(1) {
        Some(Expr::Str(code)) => Some(ParsedInteractiveSpec::StringCode(code.clone())),
        Some(form) => Some(ParsedInteractiveSpec::Form(form.clone())),
        None => Some(ParsedInteractiveSpec::NoArgs),
    }
}

fn parsed_interactive_spec_from_lambda(lambda: &LambdaData) -> Option<ParsedInteractiveSpec> {
    lambda
        .body
        .get(lambda_body_metadata_end(&lambda.body))
        .and_then(parse_interactive_spec)
}

fn interactive_form_value_to_args(value: Value) -> Result<Vec<Value>, Flow> {
    if value.is_nil() {
        return Ok(Vec::new());
    }
    if let Some(values) = value_list_to_vec(&value) {
        return Ok(values);
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("listp"), value],
    ))
}

fn parse_interactive_prefix_flags(mut line: &str) -> (Vec<char>, &str) {
    let mut flags = Vec::new();
    while let Some(ch) = line.chars().next() {
        if matches!(ch, '*' | '@' | '^') {
            flags.push(ch);
            line = &line[ch.len_utf8()..];
        } else {
            break;
        }
    }
    (flags, line)
}

fn parse_interactive_code_entries(code: &str) -> ParsedInteractiveStringCode {
    let mut parsed = ParsedInteractiveStringCode::default();
    if code.is_empty() {
        return parsed;
    }

    for (index, raw_line) in code.split('\n').enumerate() {
        let line = if index == 0 {
            let (flags, stripped) = parse_interactive_prefix_flags(raw_line);
            parsed.prefix_flags = flags;
            stripped
        } else {
            raw_line
        };
        if line.is_empty() {
            continue;
        }
        let mut chars = line.chars();
        let Some(letter) = chars.next() else {
            continue;
        };
        parsed.entries.push((letter, chars.collect::<String>()));
    }
    parsed
}

fn invalid_interactive_control_letter_error(letter: char) -> Flow {
    let codepoint = letter as u32;
    signal(
        "error",
        vec![Value::string(format!(
            "Invalid control letter \u{2018}{letter}\u{2019} (#o{codepoint:o}, #x{codepoint:04x}) in interactive calling string"
        ))],
    )
}

fn interactive_args_from_string_code(
    eval: &mut Evaluator,
    code: &str,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
) -> Result<Option<Vec<Value>>, Flow> {
    let parsed = parse_interactive_code_entries(code);
    interactive_apply_prefix_flags(eval, &parsed.prefix_flags)?;
    if parsed.entries.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut args = Vec::new();
    for (letter, prompt) in parsed.entries {
        match letter {
            'a' => args.push(super::minibuffer::builtin_read_command(
                eval,
                vec![Value::string(prompt)],
            )?),
            'b' => args.push(super::minibuffer::builtin_read_buffer(
                eval,
                vec![Value::string(prompt), Value::Nil, Value::True],
            )?),
            'B' => args.push(super::minibuffer::builtin_read_buffer(
                eval,
                vec![Value::string(prompt), Value::Nil, Value::Nil],
            )?),
            'c' => args.push(super::reader::builtin_read_char(
                eval,
                vec![Value::string(prompt)],
            )?),
            'C' => args.push(super::minibuffer::builtin_read_command(
                eval,
                vec![Value::string(prompt)],
            )?),
            'd' => args.push(interactive_point_arg(eval)?),
            'D' => args.push(super::minibuffer::builtin_read_directory_name(
                eval,
                vec![Value::string(prompt)],
            )?),
            'e' => {
                if let Some(event) = interactive_next_event_with_parameters(eval, context) {
                    args.push(event);
                } else {
                    return Err(signal(
                        "error",
                        vec![Value::string(
                            "command must be bound to an event with parameters",
                        )],
                    ));
                }
            }
            'f' => args.push(super::minibuffer::builtin_read_file_name(
                eval,
                vec![Value::string(prompt), Value::Nil, Value::Nil, Value::True],
            )?),
            'F' => args.push(super::minibuffer::builtin_read_file_name(
                eval,
                vec![Value::string(prompt)],
            )?),
            'G' => args.push(super::minibuffer::builtin_read_file_name(
                eval,
                vec![Value::string(prompt)],
            )?),
            'i' => args.push(Value::Nil),
            'k' => args.push(super::reader::builtin_read_key_sequence(
                eval,
                vec![Value::string(prompt)],
            )?),
            'K' => args.push(super::reader::builtin_read_key_sequence_vector(
                eval,
                vec![Value::string(prompt)],
            )?),
            'M' => args.push(super::reader::builtin_read_string(
                eval,
                vec![Value::string(prompt)],
            )?),
            'm' => args.push(interactive_mark_arg(eval)?),
            'N' => {
                let raw = interactive_prefix_raw_arg(eval, kind);
                if raw.is_nil() {
                    args.push(super::reader::builtin_read_number(
                        eval,
                        vec![Value::string(prompt)],
                    )?);
                } else {
                    args.push(Value::Int(prefix_numeric_value(&raw)));
                }
            }
            'p' => args.push(interactive_prefix_numeric_arg(eval, kind)),
            'P' => args.push(interactive_prefix_raw_arg(eval, kind)),
            'r' => args.extend(interactive_region_args(eval, "error")?),
            'S' => {
                let sym_name =
                    super::reader::builtin_read_string(eval, vec![Value::string(prompt)])?;
                if let Some(name) = sym_name.as_str() {
                    args.push(Value::symbol(name));
                } else {
                    return Ok(None);
                }
            }
            's' => args.push(super::reader::builtin_read_string(
                eval,
                vec![Value::string(prompt)],
            )?),
            'n' => args.push(super::reader::builtin_read_number(
                eval,
                vec![Value::string(prompt)],
            )?),
            'x' => args.push(interactive_read_expression_arg(eval, prompt)?),
            'X' => {
                let expr_value = interactive_read_expression_arg(eval, prompt)?;
                args.push(eval.eval_value(&expr_value)?);
            }
            'U' => args.push(Value::Nil),
            'v' => args.push(super::minibuffer::builtin_read_variable(
                eval,
                vec![Value::string(prompt)],
            )?),
            'z' => args.push(super::lread::builtin_read_coding_system(vec![
                Value::string(prompt),
            ])?),
            'Z' => args.push(interactive_read_coding_system_optional_arg(prompt)?),
            _ => return Err(invalid_interactive_control_letter_error(letter)),
        }
    }

    Ok(Some(args))
}

fn resolve_interactive_invocation_args(
    eval: &mut Evaluator,
    resolved_name: &str,
    func: &Value,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
) -> Result<Vec<Value>, Flow> {
    if let Some(code) = eval
        .interactive
        .get_spec(resolved_name)
        .map(|spec| spec.code.clone())
    {
        if let Some(args) = interactive_args_from_string_code(eval, &code, kind, context)? {
            return Ok(args);
        }
    }

    if let Some(lambda) = func.get_lambda_data() {
        if let Some(spec) = parsed_interactive_spec_from_lambda(lambda) {
            let maybe_args = match spec {
                ParsedInteractiveSpec::NoArgs => Some(Vec::new()),
                ParsedInteractiveSpec::StringCode(code) => {
                    interactive_args_from_string_code(eval, &code, kind, context)?
                }
                ParsedInteractiveSpec::Form(form) => {
                    let value = eval.eval(&form)?;
                    Some(interactive_form_value_to_args(value)?)
                }
            };
            if let Some(args) = maybe_args {
                return Ok(args);
            }
        }
    }

    // For bytecoded functions: extract interactive spec from closure slot 5
    // (mirrors GNU Emacs CLOSURE_INTERACTIVE handling in callint.c)
    if let Some(bc) = func.get_bytecode_data() {
        if let Some(spec) = &bc.interactive {
            // If it's a vector [spec, modes], extract just the spec
            let spec_val = if let Value::Vector(vid) = spec {
                super::value::with_heap(|h| {
                    if h.vector_len(*vid) > 0 {
                        h.vector_ref(*vid, 0)
                    } else {
                        *spec
                    }
                })
            } else {
                *spec
            };
            if let Some(s) = spec_val.as_str() {
                // String interactive spec — parse as code letters
                if let Some(args) = interactive_args_from_string_code(eval, s, kind, context)? {
                    return Ok(args);
                }
            } else if spec_val.is_nil() {
                // (interactive) with no args
                return Ok(Vec::new());
            } else {
                // Non-string spec — evaluate as a form (like GNU Feval(specs, env))
                let value = eval.eval_value(&spec_val)?;
                return Ok(interactive_form_value_to_args(value)?);
            }
        }
    }

    match kind {
        CommandInvocationKind::CallInteractively => {
            default_call_interactively_args(eval, resolved_name)
        }
        CommandInvocationKind::CommandExecute => default_command_execute_args(eval, resolved_name),
    }
}

fn value_is_lambda_form(value: &Value) -> bool {
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    items.first().and_then(Value::as_symbol_name) == Some("lambda")
}

fn normalize_command_callable(eval: &mut Evaluator, value: Value) -> Result<Value, Flow> {
    if value_is_lambda_form(&value) {
        return eval.eval_value(&value);
    }
    Ok(value)
}

fn default_command_execute_args(eval: &Evaluator, name: &str) -> Result<Vec<Value>, Flow> {
    match name {
        "self-insert-command"
        | "delete-char"
        | "kill-word"
        | "backward-kill-word"
        | "downcase-word"
        | "upcase-word"
        | "capitalize-word"
        | "transpose-lines"
        | "transpose-paragraphs"
        | "transpose-sentences"
        | "transpose-sexps"
        | "transpose-words"
        | "forward-char"
        | "backward-char"
        | "beginning-of-line"
        | "end-of-line"
        | "move-beginning-of-line"
        | "move-end-of-line"
        | "forward-word"
        | "backward-word"
        | "forward-paragraph"
        | "backward-paragraph"
        | "forward-sentence"
        | "backward-sentence"
        | "forward-sexp"
        | "backward-sexp"
        | "scroll-up-command"
        | "scroll-down-command"
        | "recenter-top-bottom"
        | "other-window" => Ok(vec![Value::Int(1)]),
        "kill-region" => interactive_region_args(eval, "user-error"),
        "kill-ring-save" => interactive_region_args(eval, "error"),
        "copy-region-as-kill" => interactive_region_args(eval, "error"),
        "set-mark-command" => Ok(vec![Value::Nil]),
        "split-window-below" | "split-window-right" => {
            // (interactive `(,(when current-prefix-arg ...) ,(selected-window)))
            let win = eval
                .frames
                .selected_frame()
                .map(|f| Value::Window(f.selected_window.0))
                .unwrap_or(Value::Nil);
            Ok(vec![Value::Nil, win])
        }
        "capitalize-region" => interactive_region_args(eval, "error"),
        "upcase-initials-region" => interactive_region_args(eval, "error"),
        "upcase-region" | "downcase-region" => Err(signal(
            "args-out-of-range",
            vec![Value::string(""), Value::Int(0)],
        )),
        _ => Ok(Vec::new()),
    }
}

fn default_call_interactively_args(eval: &Evaluator, name: &str) -> Result<Vec<Value>, Flow> {
    match name {
        "self-insert-command"
        | "delete-char"
        | "kill-word"
        | "backward-kill-word"
        | "downcase-word"
        | "upcase-word"
        | "capitalize-word"
        | "transpose-lines"
        | "transpose-paragraphs"
        | "transpose-sentences"
        | "transpose-sexps"
        | "transpose-words"
        | "forward-char"
        | "backward-char"
        | "beginning-of-line"
        | "end-of-line"
        | "move-beginning-of-line"
        | "move-end-of-line" => Ok(vec![interactive_prefix_numeric_arg(
            eval,
            CommandInvocationKind::CallInteractively,
        )]),
        "set-mark-command" => Ok(vec![
            dynamic_or_global_symbol_value(eval, "current-prefix-arg").unwrap_or(Value::Nil),
        ]),
        "upcase-region" | "downcase-region" | "capitalize-region" => {
            interactive_region_args(eval, "error")
        }
        _ => default_command_execute_args(eval, name),
    }
}

fn resolve_command_target(eval: &Evaluator, designator: &Value) -> Option<(String, Value)> {
    if let Some(name) = designator.as_symbol_name() {
        if let Some((resolved_name, value)) = resolve_function_designator_symbol(eval, name) {
            return Some((resolved_name, value));
        }
        if builtin_command_name(name) {
            return Some((name.to_string(), Value::Subr(intern(name))));
        }
        return None;
    }
    match designator {
        Value::Subr(id) => Some((resolve_sym(*id).to_owned(), *designator)),
        Value::True => Some(("t".to_string(), *designator)),
        Value::Keyword(id) => Some((resolve_sym(*id).to_owned(), *designator)),
        _ => Some(("<anonymous>".to_string(), *designator)),
    }
}

fn last_command_event_char(eval: &Evaluator) -> Option<char> {
    let event = dynamic_or_global_symbol_value(eval, "last-command-event")?;
    match event {
        Value::Char(c) => Some(c),
        Value::Int(n) if n >= 0 => char::from_u32(n as u32),
        _ => None,
    }
}

/// `(self-insert-command N &optional NOAUTOFILL)` -- insert the last typed
/// character N times.
pub(crate) fn builtin_self_insert_command(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("self-insert-command"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let repeats = match args[0] {
        Value::Int(n) => n,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), args[0]],
            ));
        }
    };
    if repeats < 0 {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Negative repetition argument {}",
                repeats
            ))],
        ));
    }
    if repeats == 0 {
        return Ok(Value::Nil);
    }
    if args.get(1).is_some_and(|v| !v.is_nil()) {
        return Ok(Value::Nil);
    }

    let Some(ch) = last_command_event_char(eval) else {
        return Ok(Value::Nil);
    };
    let Some(repeat_count) = usize::try_from(repeats).ok() else {
        return Ok(Value::Nil);
    };
    let mut text = String::new();
    for _ in 0..repeat_count {
        text.push(ch);
    }
    if let Some(current_id) = eval.buffers.current_buffer_id() {
        let _ = eval.buffers.insert_into_buffer(current_id, &text);
    }
    Ok(Value::Nil)
}

/// `(keyboard-quit)` -- cancel the current command sequence.
pub(crate) fn builtin_keyboard_quit(_eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("keyboard-quit", &args, 0)?;
    Err(signal("quit", vec![]))
}

/// `(key-binding KEY &optional ACCEPT-DEFAULTS NO-REMAP POSITION)`
/// Return the binding for KEY in the current keymaps.
pub(crate) fn builtin_key_binding(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("key-binding", &args, 1)?;
    expect_max_args("key-binding", &args, 4)?;
    let string_designator = args[0].is_string();
    let no_remap = args.get(2).is_some_and(|v| v.is_truthy());
    if let Some(position) = args.get(3) {
        interactive_validate_integer_position_arg(eval, position)?;
    }

    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(super::kbd::KeyDesignatorError::WrongType(other)) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), other],
            ));
        }
        Err(super::kbd::KeyDesignatorError::Parse(_)) => {
            return Ok(Value::Nil);
        }
    };
    if events.is_empty() {
        if !string_designator {
            return Ok(Value::Nil);
        }
        let global = ensure_global_keymap(eval);
        let mut maps = Vec::new();
        if !eval.current_local_map.is_nil() {
            maps.push(eval.current_local_map);
        }
        maps.push(global);
        return Ok(Value::list(maps));
    }

    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();

    if let Some(value) = key_binding_lookup_in_minor_mode_maps(eval, &emacs_events) {
        return Ok(key_binding_apply_remap(eval, value, no_remap));
    }

    // Try local map first, then global.
    if !eval.current_local_map.is_nil() {
        if let Some(value) =
            key_binding_lookup_in_keymap(eval, &eval.current_local_map, &emacs_events)
        {
            return Ok(key_binding_apply_remap(eval, value, no_remap));
        }
    }

    let global = get_global_keymap(eval);
    if !global.is_nil() {
        if let Some(value) = key_binding_lookup_in_keymap(eval, &global, &emacs_events) {
            return Ok(key_binding_apply_remap(eval, value, no_remap));
        }
    }
    // Fallback: unbound printable chars default to self-insert-command
    if events.len() == 1 && is_plain_printable_char_event(&events[0]) {
        return Ok(Value::symbol("self-insert-command"));
    }

    Ok(Value::Nil)
}

fn interactive_validate_integer_position_arg(
    eval: &Evaluator,
    position: &Value,
) -> Result<(), Flow> {
    let Value::Int(pos) = position else {
        return Ok(());
    };
    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(());
    };
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if *pos < point_min || *pos > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Buffer(buf.id), Value::Int(*pos)],
        ));
    }
    Ok(())
}

/// `(local-key-binding KEY &optional ACCEPT-DEFAULTS)`
pub(crate) fn builtin_local_key_binding(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("local-key-binding", &args, 1)?;
    expect_max_args("local-key-binding", &args, 2)?;

    if eval.current_local_map.is_nil() {
        return Ok(Value::Nil);
    }

    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(super::kbd::KeyDesignatorError::WrongType(other)) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), other],
            ));
        }
        Err(super::kbd::KeyDesignatorError::Parse(_)) => {
            return Ok(Value::Nil);
        }
    };
    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();
    Ok(lookup_keymap_with_partial(
        &eval.current_local_map,
        &emacs_events,
    ))
}

fn minor_mode_map_entry(entry: &Value) -> Option<(String, Value)> {
    let Value::Cons(cell) = entry else {
        return None;
    };

    let (mode, cdr) = {
        let pair = read_cons(*cell);
        (pair.car, pair.cdr)
    };
    let mode_name = mode.as_symbol_name()?.to_string();
    // The standard entry format is (MODE . KEYMAP) — a dotted pair.
    // CDR is the keymap directly. With cons-list keymaps, CDR is a
    // Value::Cons, so we return it as-is. The caller checks is_list_keymap.
    if cdr == Value::Nil {
        return None;
    }
    Some((mode_name, cdr))
}

/// Look up a key sequence in a keymap Value, returning the binding if found.
fn key_binding_lookup_in_keymap(
    eval: &Evaluator,
    keymap: &Value,
    events: &[Value],
) -> Option<Value> {
    if !is_list_keymap(keymap) {
        return None;
    }
    if events.is_empty() {
        return None;
    }
    // Walk events one at a time, resolving prefix symbols through the
    // obarray so that e.g. the `Control-X-prefix` symbol is followed
    // into `ctl-x-map`.
    let mut current_map = *keymap;
    for (i, event) in events.iter().enumerate() {
        let binding = list_keymap_lookup_one(&current_map, event);
        if binding.is_nil() {
            return None;
        }
        if i == events.len() - 1 {
            // Final event — return the binding (command/lambda/etc.)
            return Some(binding);
        }
        // Intermediate event — must resolve to a prefix keymap.
        if is_list_keymap(&binding) {
            current_map = binding;
        } else if let Some(sym_name) = binding.as_symbol_name() {
            // Resolve symbol function cell to a keymap
            if let Some(func) = eval.obarray.symbol_function(sym_name).copied() {
                if is_list_keymap(&func) {
                    current_map = func;
                    continue;
                }
            }
            return None; // Symbol doesn't resolve to a keymap
        } else {
            return None; // Non-keymap, non-symbol binding in prefix position
        }
    }
    None
}

/// Get the global keymap Value from obarray (without creating one).
fn get_global_keymap(eval: &Evaluator) -> Value {
    eval.obarray
        .symbol_value("global-map")
        .copied()
        .unwrap_or(Value::Nil)
}

/// Get the global keymap, creating one if it doesn't exist.
fn ensure_global_keymap(eval: &mut Evaluator) -> Value {
    if let Some(val) = eval.obarray.symbol_value("global-map").copied() {
        if is_list_keymap(&val) {
            return val;
        }
    }
    let km = make_list_keymap();
    eval.obarray.set_symbol_value("global-map", km);
    km
}

fn key_binding_apply_remap(eval: &Evaluator, binding: Value, no_remap: bool) -> Value {
    if no_remap {
        return binding;
    }
    let Some(command_name) = binding.as_symbol_name().map(ToString::to_string) else {
        return binding;
    };
    match command_remapping_lookup_in_active_keymaps(eval, &command_name) {
        Some(remapped) if !remapped.is_nil() => remapped,
        _ => binding,
    }
}

fn key_binding_lookup_in_minor_mode_alist(
    eval: &Evaluator,
    events: &[Value],
    alist_value: &Value,
) -> Option<Value> {
    let entries = list_to_vec(alist_value)?;
    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_or_global_symbol_value(eval, &mode_name).is_some_and(|v| v.is_truthy()) {
            continue;
        }

        if !is_list_keymap(&map_value) {
            continue;
        }

        if let Some(binding) = key_binding_lookup_in_keymap(eval, &map_value, events) {
            return Some(binding);
        }
    }
    None
}

fn key_binding_lookup_in_minor_mode_maps(eval: &Evaluator, events: &[Value]) -> Option<Value> {
    if let Some(emulation_raw) = dynamic_or_global_symbol_value(eval, "emulation-mode-map-alists") {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value(eval, name).unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some(value) =
                    key_binding_lookup_in_minor_mode_alist(eval, events, &alist_value)
                {
                    return Some(value);
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) = dynamic_or_global_symbol_value(eval, alist_name) else {
            continue;
        };
        if let Some(value) = key_binding_lookup_in_minor_mode_alist(eval, events, &alist_value) {
            return Some(value);
        }
    }
    None
}

fn lookup_minor_mode_binding_in_alist(
    eval: &Evaluator,
    events: &[KeyEvent],
    alist_value: &Value,
) -> Result<Option<(String, Value)>, Flow> {
    let Some(entries) = list_to_vec(alist_value) else {
        return Ok(None);
    };

    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_or_global_symbol_value(eval, &mode_name).is_some_and(|v| v.is_truthy()) {
            continue;
        }

        // Resolve the keymap value - could be a keymap directly or a symbol
        let keymap = if is_list_keymap(&map_value) {
            map_value
        } else if let Some(sym_name) = map_value.as_symbol_name() {
            match eval.obarray.symbol_value(sym_name).copied() {
                Some(v) if is_list_keymap(&v) => v,
                _ => match eval.obarray.symbol_function(sym_name).copied() {
                    Some(v) if is_list_keymap(&v) => v,
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("keymapp"), map_value],
                        ));
                    }
                },
            }
        } else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("keymapp"), map_value],
            ));
        };

        let binding = lookup_keymap_with_partial_value(&keymap, events);
        if binding.is_nil() {
            continue;
        }

        return Ok(Some((mode_name, binding)));
    }

    Ok(None)
}

/// `(minor-mode-key-binding KEY &optional ACCEPT-DEFAULTS)`
/// Look up KEY in active minor mode keymaps.
pub(crate) fn builtin_minor_mode_key_binding(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("minor-mode-key-binding", &args, 1)?;
    expect_max_args("minor-mode-key-binding", &args, 2)?;

    // Emacs returns nil (not a type error) for non-array key designators here.
    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(_) => return Ok(Value::Nil),
    };

    if let Some(emulation_raw) = dynamic_or_global_symbol_value(eval, "emulation-mode-map-alists") {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value(eval, name).unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some((mode_name, binding)) =
                    lookup_minor_mode_binding_in_alist(eval, &events, &alist_value)?
                {
                    return Ok(Value::list(vec![Value::cons(
                        Value::symbol(mode_name),
                        binding,
                    )]));
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) = dynamic_or_global_symbol_value(eval, alist_name) else {
            continue;
        };
        if let Some((mode_name, binding)) =
            lookup_minor_mode_binding_in_alist(eval, &events, &alist_value)?
        {
            return Ok(Value::list(vec![Value::cons(
                Value::symbol(mode_name),
                binding,
            )]));
        }
    }

    Ok(Value::Nil)
}

/// `(where-is-internal DEFINITION &optional KEYMAP FIRSTONLY NOINDIRECT NO-REMAP)`
/// Return list of key sequences that invoke DEFINITION.
pub(crate) fn builtin_where_is_internal(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("where-is-internal", &args, 1)?;
    expect_max_args("where-is-internal", &args, 5)?;

    let definition = &args[0];
    let first_only = args.get(2).is_some_and(|v| !v.is_nil());

    let keymaps = if let Some(keymap_arg) = args.get(1) {
        if keymap_arg.is_nil() {
            let gm = get_global_keymap(eval);
            if !is_list_keymap(&gm) {
                return Ok(Value::Nil);
            }
            vec![gm]
        } else {
            where_is_internal_keymaps(eval, keymap_arg)?
        }
    } else {
        let gm = get_global_keymap(eval);
        if !is_list_keymap(&gm) {
            return Ok(Value::Nil);
        }
        vec![gm]
    };

    let mut sequences = Vec::new();
    for keymap in &keymaps {
        let mut prefix = Vec::new();
        if collect_where_is_sequences_value(
            keymap,
            definition,
            &mut prefix,
            &mut sequences,
            first_only,
            0,
        ) && first_only
        {
            break;
        }
    }

    if sequences.is_empty() {
        return Ok(Value::Nil);
    }

    if first_only {
        // Convert Vec<Value> events to a vector value
        return Ok(Value::vector(sequences[0].clone()));
    }
    let out: Vec<Value> = sequences
        .iter()
        .map(|seq| Value::vector(seq.clone()))
        .collect();
    Ok(Value::list(out))
}

/// `(this-command-keys)` -> string of keys that invoked current command.
pub(crate) fn builtin_this_command_keys(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("this-command-keys", &args, 0)?;
    let read_keys = eval.read_command_keys();
    if !read_keys.is_empty() {
        if let Some(rendered) = command_key_events_to_string(read_keys) {
            return Ok(Value::string(rendered));
        }
        return Ok(Value::vector(read_keys.to_vec()));
    }

    let keys = eval.interactive.this_command_keys();
    Ok(Value::string(keys.join(" ")))
}

/// `(this-command-keys-vector)` -> vector of keys that invoked current command.
pub(crate) fn builtin_this_command_keys_vector(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-command-keys-vector", &args, 0)?;
    let read_keys = eval.read_command_keys();
    if !read_keys.is_empty() {
        return Ok(Value::vector(read_keys.to_vec()));
    }

    let keys = eval.interactive.this_command_keys();
    let vals: Vec<Value> = keys.iter().map(|k| Value::string(k.clone())).collect();
    Ok(Value::vector(vals))
}

/// `(clear-this-command-keys &optional KEEP-RECORD)` -> nil.
///
/// Clears current command-key context used by `this-command-keys*`.
/// When KEEP-RECORD is nil or omitted, also clears recent input history used
/// by `recent-keys`.
pub(crate) fn builtin_clear_this_command_keys(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("clear-this-command-keys", &args, 1)?;
    let keep_record = args.first().is_some_and(|arg| arg.is_truthy());
    eval.clear_read_command_keys();
    eval.interactive.set_this_command_keys(Vec::new());
    if !keep_record {
        eval.clear_recent_input_events();
    }
    Ok(Value::Nil)
}

fn command_key_events_to_string(events: &[Value]) -> Option<String> {
    let mut out = String::new();
    for event in events {
        let ch = match event {
            Value::Char(c) => *c,
            Value::Int(n) if *n >= 0 => char::from_u32(*n as u32)?,
            _ => return None,
        };
        out.push(ch);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Special forms for mode definition (called from eval.rs)
// ---------------------------------------------------------------------------

/// `(define-minor-mode MODE DOC &rest BODY)` with keyword args
/// :lighter :keymap :global
///
/// Expands to:
///   - defvar MODE (the toggle variable)
///   - defun MODE (the toggle function)
///   - registers the minor mode in ModeRegistry
pub(crate) fn sf_define_minor_mode(eval: &mut Evaluator, tail: &[Expr]) -> EvalResult {
    if tail.len() < 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("define-minor-mode"),
                Value::Int(tail.len() as i64),
            ],
        ));
    }

    let Expr::Symbol(mode_name_id) = &tail[0] else {
        return Err(signal("wrong-type-argument", vec![]));
    };
    let mode_name = resolve_sym(*mode_name_id);

    // Parse keyword arguments from the tail
    let mut lighter: Option<String> = None;
    let mut keymap_name: Option<String> = None;
    let mut global = false;
    let mut body_start = 2; // skip mode name and docstring

    // Skip docstring if present
    if tail.len() > 1 {
        if let Expr::Str(_) = &tail[1] {
            body_start = 2;
        } else {
            body_start = 1; // no docstring
        }
    }

    // Parse keyword args
    let mut i = body_start;
    while i + 1 < tail.len() {
        match &tail[i] {
            Expr::Keyword(id) if resolve_sym(*id) == ":lighter" => {
                if let Expr::Str(s) = &tail[i + 1] {
                    lighter = Some(s.clone());
                }
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":keymap" => {
                if let Expr::Symbol(sid) = &tail[i + 1] {
                    keymap_name = Some(resolve_sym(*sid).to_owned());
                }
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":global" => {
                match &tail[i + 1] {
                    Expr::Bool(true) => {
                        global = true;
                    }
                    Expr::Symbol(sid) if resolve_sym(*sid) == "t" => {
                        global = true;
                    }
                    _ => {}
                }
                i += 2;
            }
            _ => break,
        }
    }

    let body_forms = &tail[i..];

    // 1. Create the toggle variable (defvar MODE nil)
    eval.obarray.set_symbol_value(mode_name, Value::Nil);
    eval.obarray.make_special(mode_name);

    // 2. Register with ModeRegistry
    let mode = MinorMode {
        name: mode_name.to_owned(),
        lighter: lighter.clone(),
        keymap_name: keymap_name.clone(),
        global,
        body: None,
    };
    eval.modes.register_minor_mode(mode);

    // 3. Register as an interactive command
    eval.interactive
        .register_interactive(mode_name, InteractiveSpec::no_args());

    // 4. Create toggle function that:
    //    - Toggles the variable
    //    - Runs the body forms
    //    - Returns the new value
    let mode_name_sym = *mode_name_id;
    let _global_flag = global;

    // Build a lambda body for the toggle function
    // We synthesize: (progn (setq MODE (not MODE)) BODY...)
    let toggle_body_exprs: Vec<Expr> = {
        let mut exprs = Vec::new();
        // (setq MODE (not MODE))
        exprs.push(Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(mode_name_sym),
            Expr::List(vec![
                Expr::Symbol(intern("not")),
                Expr::Symbol(mode_name_sym),
            ]),
        ]));
        // Append body forms
        for form in body_forms {
            exprs.push(form.clone());
        }
        // Return the mode variable value
        exprs.push(Expr::Symbol(mode_name_sym));
        exprs
    };

    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: toggle_body_exprs.into(),
        env: None,
        docstring: None,
        doc_form: None,
    });

    eval.obarray.set_symbol_function(mode_name, lambda);

    Ok(Value::symbol(mode_name))
}

/// `(define-derived-mode MODE PARENT NAME DOC &rest BODY)` with keyword args
/// :syntax-table :abbrev-table
///
/// Creates a major mode that derives from PARENT.
#[derive(Clone, Debug)]
enum DerivedModeVarSpec {
    None,
    Existing(String),
    Declared(String),
}

impl DerivedModeVarSpec {
    fn runtime_name(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Existing(name) | Self::Declared(name) => Some(name),
        }
    }

    fn declared_name(&self) -> Option<&str> {
        match self {
            Self::Declared(name) => Some(name),
            Self::None | Self::Existing(_) => None,
        }
    }
}

fn maybe_macroexpand_derived_mode_body(eval: &mut Evaluator, forms: Vec<Expr>) -> Vec<Expr> {
    let Some(macroexpand_fn) = eval
        .obarray()
        .symbol_function("internal-macroexpand-for-load")
        .cloned()
    else {
        return forms;
    };

    if eval
        .obarray()
        .symbol_function("`--pcase-macroexpander")
        .is_none()
    {
        return forms;
    }

    let saved_roots = eval.save_temp_roots();
    let mut expanded_forms = Vec::with_capacity(forms.len());
    for form in forms {
        let form_value = quote_to_value(&form);
        eval.push_temp_root(form_value);
        eval.push_temp_root(macroexpand_fn);
        let expanded = match eval.apply(macroexpand_fn, vec![form_value, Value::True]) {
            Ok(value) => value_to_expr(&value),
            Err(_) => form.clone(),
        };
        eval.restore_temp_roots(saved_roots);
        expanded_forms.push(expanded);
    }
    eval.restore_temp_roots(saved_roots);
    expanded_forms
}

pub(crate) fn sf_define_derived_mode(eval: &mut Evaluator, tail: &[Expr]) -> EvalResult {
    if tail.len() < 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("define-derived-mode"),
                Value::Int(tail.len() as i64),
            ],
        ));
    }

    let Expr::Symbol(mode_name_id) = &tail[0] else {
        return Err(signal("wrong-type-argument", vec![]));
    };
    let mode_name = resolve_sym(*mode_name_id);
    let sym = |name: &str| Expr::Symbol(intern(name));
    let quote_sym = |name: &str| {
        Expr::List(vec![
            Expr::Symbol(intern("quote")),
            Expr::Symbol(intern(name)),
        ])
    };

    // Parent can be nil or a symbol
    let parent = match &tail[1] {
        Expr::Symbol(id) if matches!(resolve_sym(*id), "nil" | "fundamental-mode") => None,
        Expr::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        _ => None,
    };

    // Pretty name (3rd arg) - evaluate it
    let pretty_name = match &tail[2] {
        Expr::Str(s) => s.clone(),
        _ => {
            let val = eval.eval(&tail[2])?;
            match val.as_str() {
                Some(s) => s.to_string(),
                None => mode_name.to_owned(),
            }
        }
    };

    // Parse optional keyword args and body
    let mut syntax_spec = DerivedModeVarSpec::Declared(format!("{mode_name}-syntax-table"));
    let mut abbrev_spec = DerivedModeVarSpec::Declared(format!("{mode_name}-abbrev-table"));
    let mut run_interactively = true;
    let mut after_hook: Option<Expr> = None;
    let mut body_start = 3;

    // Skip docstring if present
    if tail.len() > 3 {
        if let Expr::Str(_) = &tail[3] {
            body_start = 4;
        }
    }

    // Parse keyword args
    let mut i = body_start;
    while i + 1 < tail.len() {
        match &tail[i] {
            Expr::Keyword(id) if resolve_sym(*id) == ":syntax-table" => {
                syntax_spec = match &tail[i + 1] {
                    Expr::Symbol(sid) if resolve_sym(*sid) == "nil" => DerivedModeVarSpec::None,
                    Expr::Symbol(sid) => DerivedModeVarSpec::Existing(resolve_sym(*sid).to_owned()),
                    _ => DerivedModeVarSpec::None,
                };
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":abbrev-table" => {
                abbrev_spec = match &tail[i + 1] {
                    Expr::Symbol(sid) if resolve_sym(*sid) == "nil" => DerivedModeVarSpec::None,
                    Expr::Symbol(sid) => DerivedModeVarSpec::Existing(resolve_sym(*sid).to_owned()),
                    _ => DerivedModeVarSpec::None,
                };
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":interactive" => {
                run_interactively =
                    !matches!(&tail[i + 1], Expr::Symbol(sid) if resolve_sym(*sid) == "nil");
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":after-hook" => {
                after_hook = Some(tail[i + 1].clone());
                i += 2;
            }
            Expr::Keyword(id) if resolve_sym(*id) == ":group" => {
                i += 2;
            }
            _ => break,
        }
    }

    let body_forms = &tail[i..];

    // Derive hook name and keymap name
    let hook_name = format!("{}-hook", mode_name);
    let keymap_name = format!("{}-map", mode_name);

    eval.obarray.set_symbol_value(&hook_name, Value::Nil);
    eval.obarray.make_special(&hook_name);

    if !eval.obarray.boundp(&keymap_name) {
        eval.obarray
            .set_symbol_value(&keymap_name, make_sparse_list_keymap());
    }
    if let Some(name) = syntax_spec.declared_name() {
        if !eval.obarray.boundp(name) {
            let table = super::syntax::builtin_make_syntax_table(vec![])?;
            eval.obarray.set_symbol_value(name, table);
        }
    }
    if let Some(name) = abbrev_spec.declared_name() {
        if !eval.obarray.boundp(name) {
            super::abbrev::builtin_define_abbrev_table(
                eval,
                vec![Value::symbol(name), Value::Nil],
            )?;
        }
    }
    if let Some(ref parent_name) = parent {
        eval.obarray
            .put_property(mode_name, "derived-mode-parent", Value::symbol(parent_name));
    }

    // 1. Register the major mode
    let mode = MajorMode {
        name: mode_name.to_owned(),
        pretty_name: pretty_name.clone(),
        parent: parent.clone(),
        mode_hook: hook_name.clone(),
        keymap_name: Some(keymap_name.clone()),
        syntax_table_name: syntax_spec.runtime_name().map(str::to_string),
        abbrev_table_name: abbrev_spec.runtime_name().map(str::to_string),
        font_lock: None,
        body: None,
    };
    eval.modes.register_major_mode(mode);

    // 2. Register as interactive command
    if run_interactively {
        eval.interactive
            .register_interactive(mode_name, InteractiveSpec::no_args());
    }

    // 3. Create mode function that mirrors GNU Emacs's generated
    // `define-derived-mode` form closely enough for startup/runtime parity.
    let mut func_body: Vec<Expr> = Vec::new();

    // Run the parent (or reset locals for a root mode) first.
    if let Some(ref par) = parent {
        func_body.push(Expr::List(vec![sym(par)]));
    } else {
        func_body.push(Expr::List(vec![sym("kill-all-local-variables")]));
    }

    // (setq major-mode 'MODE)
    func_body.push(Expr::List(vec![
        sym("setq"),
        sym("major-mode"),
        quote_sym(mode_name),
    ]));

    // (setq mode-name PRETTY-NAME)
    func_body.push(Expr::List(vec![
        sym("setq"),
        sym("mode-name"),
        Expr::Str(pretty_name),
    ]));

    if parent.is_some() {
        func_body.push(Expr::List(vec![
            sym("if"),
            Expr::List(vec![sym("keymap-parent"), sym(&keymap_name)]),
            Expr::Symbol(intern("nil")),
            Expr::List(vec![
                sym("set-keymap-parent"),
                sym(&keymap_name),
                Expr::List(vec![sym("current-local-map")]),
            ]),
        ]));

        if let Some(name) = syntax_spec.declared_name() {
            func_body.push(Expr::List(vec![
                sym("let"),
                Expr::List(vec![Expr::List(vec![
                    sym("parent"),
                    Expr::List(vec![sym("char-table-parent"), sym(name)]),
                ])]),
                Expr::List(vec![
                    sym("if"),
                    Expr::List(vec![
                        sym("and"),
                        sym("parent"),
                        Expr::List(vec![
                            sym("not"),
                            Expr::List(vec![
                                sym("eq"),
                                sym("parent"),
                                Expr::List(vec![sym("standard-syntax-table")]),
                            ]),
                        ]),
                    ]),
                    Expr::Symbol(intern("nil")),
                    Expr::List(vec![
                        sym("set-char-table-parent"),
                        sym(name),
                        Expr::List(vec![sym("syntax-table")]),
                    ]),
                ]),
            ]));
        }

        if let Some(name) = abbrev_spec.declared_name() {
            func_body.push(Expr::List(vec![
                sym("if"),
                Expr::List(vec![
                    sym("or"),
                    Expr::List(vec![
                        sym("abbrev-table-get"),
                        sym(name),
                        Expr::Keyword(intern(":parents")),
                    ]),
                    Expr::List(vec![sym("eq"), sym(name), sym("local-abbrev-table")]),
                ]),
                Expr::Symbol(intern("nil")),
                Expr::List(vec![
                    sym("abbrev-table-put"),
                    sym(name),
                    Expr::Keyword(intern(":parents")),
                    Expr::List(vec![sym("list"), sym("local-abbrev-table")]),
                ]),
            ]));
        }
    }

    func_body.push(Expr::List(vec![sym("use-local-map"), sym(&keymap_name)]));

    if let Some(name) = syntax_spec.runtime_name() {
        func_body.push(Expr::List(vec![sym("set-syntax-table"), sym(name)]));
    }

    if let Some(name) = abbrev_spec.runtime_name() {
        func_body.push(Expr::List(vec![
            sym("setq"),
            sym("local-abbrev-table"),
            sym(name),
        ]));
    }

    // Body forms
    for form in body_forms {
        func_body.push(form.clone());
    }

    // (run-mode-hooks 'MODE-hook)
    func_body.push(Expr::List(vec![
        sym("run-mode-hooks"),
        quote_sym(&hook_name),
    ]));

    if let Some(form) = after_hook {
        func_body.push(form);
    }

    let func_body = maybe_macroexpand_derived_mode_body(eval, func_body);

    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: func_body.into(),
        env: None,
        docstring: None,
        doc_form: None,
    });

    eval.obarray.set_symbol_function(mode_name, lambda);

    // Set up major-mode/mode-name as special buffer-oriented variables.
    if !eval.obarray.boundp("major-mode") {
        eval.obarray
            .set_symbol_value("major-mode", Value::symbol("fundamental-mode"));
    }
    eval.obarray.make_special("major-mode");
    if !eval.obarray.boundp("mode-name") {
        eval.obarray
            .set_symbol_value("mode-name", Value::string("Fundamental"));
    }
    eval.obarray.make_special("mode-name");

    if !eval.obarray.boundp("local-abbrev-table") {
        eval.obarray
            .set_symbol_value("local-abbrev-table", Value::Nil);
    }
    eval.obarray.make_special("local-abbrev-table");

    Ok(Value::symbol(mode_name))
}

/// `(define-generic-mode MODE COMMENT-LIST KEYWORD-LIST FONT-LOCK-LIST
///    AUTO-MODE-LIST FUNCTION-LIST &optional DOCSTRING)`
/// Simplified generic mode definition.
pub(crate) fn sf_define_generic_mode(eval: &mut Evaluator, tail: &[Expr]) -> EvalResult {
    if tail.len() < 5 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("define-generic-mode"),
                Value::Int(tail.len() as i64),
            ],
        ));
    }

    let Expr::Symbol(mode_name_id) = &tail[0] else {
        return Err(signal("wrong-type-argument", vec![]));
    };
    let mode_name = resolve_sym(*mode_name_id);

    // Register as a basic major mode with no parent
    let mode = MajorMode {
        name: mode_name.to_owned(),
        pretty_name: mode_name.replace('-', " "),
        parent: None,
        mode_hook: format!("{}-hook", mode_name),
        keymap_name: None,
        syntax_table_name: None,
        abbrev_table_name: None,
        font_lock: None,
        body: None,
    };
    eval.modes.register_major_mode(mode);

    // Register as interactive
    eval.interactive
        .register_interactive(mode_name, InteractiveSpec::no_args());

    // Create a simple mode function
    let lambda = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: vec![Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(intern("major-mode")),
            Expr::List(vec![
                Expr::Symbol(intern("quote")),
                Expr::Symbol(*mode_name_id),
            ]),
        ])]
        .into(),
        env: None,
        docstring: None,
        doc_form: None,
    });

    eval.obarray.set_symbol_function(mode_name, lambda);

    Ok(Value::symbol(mode_name))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_plain_printable_char_event(event: &KeyEvent) -> bool {
    matches!(
        event,
        KeyEvent::Char {
            code,
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
        } if !code.is_control() && *code != '\u{7f}'
    )
}

fn command_remapping_keymap_arg_valid(_eval: &Evaluator, value: &Value) -> bool {
    // Oracle accepts cons/list keymap-like objects in this slot, not just valid keymaps.
    // Non-keymap cons cells are silently treated as "no remap found".
    matches!(value, Value::Cons(_)) || is_list_keymap(value)
}

fn command_remapping_lookup_in_keymap_value(keymap: &Value, command_name: &str) -> Option<Value> {
    // Use the lisp keymap walker which handles the (remap keymap ...) structure
    command_remapping_lookup_in_lisp_keymap(keymap, command_name)
        .map(command_remapping_normalize_target)
}

fn command_remapping_lookup_in_minor_mode_alist(
    eval: &Evaluator,
    command_name: &str,
    alist_value: &Value,
) -> Option<Value> {
    let entries = list_to_vec(alist_value)?;
    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_or_global_symbol_value(eval, &mode_name).is_some_and(|v| v.is_truthy()) {
            continue;
        }

        if !is_list_keymap(&map_value) {
            continue;
        }

        if let Some(value) = command_remapping_lookup_in_keymap_value(&map_value, command_name) {
            return Some(value);
        }
    }
    None
}

fn command_remapping_lookup_in_minor_mode_maps(
    eval: &Evaluator,
    command_name: &str,
) -> Option<Value> {
    if let Some(emulation_raw) = dynamic_or_global_symbol_value(eval, "emulation-mode-map-alists") {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value(eval, name).unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some(value) =
                    command_remapping_lookup_in_minor_mode_alist(eval, command_name, &alist_value)
                {
                    return Some(value);
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) = dynamic_or_global_symbol_value(eval, alist_name) else {
            continue;
        };
        if let Some(value) =
            command_remapping_lookup_in_minor_mode_alist(eval, command_name, &alist_value)
        {
            return Some(value);
        }
    }

    None
}

fn command_remapping_lookup_in_active_keymaps(
    eval: &Evaluator,
    command_name: &str,
) -> Option<Value> {
    if let Some(value) = command_remapping_lookup_in_minor_mode_maps(eval, command_name) {
        return Some(value);
    }
    if is_list_keymap(&eval.current_local_map) {
        if let Some(value) =
            command_remapping_lookup_in_keymap_value(&eval.current_local_map, command_name)
        {
            return Some(value);
        }
    }
    let global_map = get_global_keymap(eval);
    if !is_list_keymap(&global_map) {
        return None;
    }
    command_remapping_lookup_in_keymap_value(&global_map, command_name)
}

fn command_remapping_command_name(command: &Value) -> Option<String> {
    Some(match command {
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        _ => return None,
    })
}

fn command_remapping_list_tail(value: &Value, n: usize) -> Option<Value> {
    let mut cursor = *value;
    for _ in 0..n {
        match cursor {
            Value::Cons(cell) => {
                cursor = {
                    let pair = read_cons(cell);
                    pair.cdr
                };
            }
            _ => return None,
        }
    }
    Some(cursor)
}

fn command_remapping_nth_list_element(value: &Value, index: usize) -> Option<Value> {
    let tail = command_remapping_list_tail(value, index)?;
    match tail {
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            Some(pair.car)
        }
        _ => None,
    }
}

fn command_remapping_lookup_in_lisp_remap_entry(
    entry: &Value,
    command_name: &str,
) -> Option<Value> {
    if command_remapping_nth_list_element(entry, 0)?.as_symbol_name() != Some("remap") {
        return None;
    }
    if command_remapping_nth_list_element(entry, 1)?.as_symbol_name() != Some("keymap") {
        return None;
    }

    let mut bindings = command_remapping_list_tail(entry, 2)?;
    while let Value::Cons(cell) = bindings {
        let (binding_entry, rest) = {
            let pair = read_cons(cell);
            (pair.car, pair.cdr)
        };
        if let Value::Cons(binding_pair) = binding_entry {
            let (binding_key, binding_target) = {
                let pair = read_cons(binding_pair);
                (pair.car, pair.cdr)
            };
            if binding_key.as_symbol_name() == Some(command_name) {
                return Some(binding_target);
            }
        }
        bindings = rest;
    }
    None
}

fn command_remapping_lookup_in_lisp_keymap(keymap: &Value, command_name: &str) -> Option<Value> {
    let mut cursor = *keymap;
    let mut first = true;
    while let Value::Cons(cell) = cursor {
        let (car, cdr) = {
            let pair = read_cons(cell);
            (pair.car, pair.cdr)
        };
        if first {
            if car.as_symbol_name() != Some("keymap") {
                return None;
            }
            first = false;
        } else if let Some(target) =
            command_remapping_lookup_in_lisp_remap_entry(&car, command_name)
        {
            return Some(target);
        }
        cursor = cdr;
    }
    None
}

fn command_remapping_menu_item_target(value: &Value) -> Option<Value> {
    let mut current = *value;
    let mut index = 0usize;
    let mut head_is_menu_item = false;
    while let Value::Cons(cell) = current {
        let (car, cdr) = {
            let pair = read_cons(cell);
            (pair.car, pair.cdr)
        };
        if index == 0 {
            head_is_menu_item = car.as_symbol_name() == Some("menu-item");
        } else if index == 2 {
            return head_is_menu_item.then_some(car);
        }
        current = cdr;
        index += 1;
    }
    None
}

fn command_remapping_normalize_target(raw: Value) -> Value {
    if let Some(menu_target) = command_remapping_menu_item_target(&raw) {
        // Oracle unwraps well-formed menu-item bindings for command remapping.
        // Integer payloads in command slot still collapse to nil.
        return if menu_target.is_integer() {
            Value::Nil
        } else {
            menu_target
        };
    }

    // Oracle treats plain integer and `t` remap targets as no-remap.
    if matches!(raw, Value::Int(_) | Value::True) {
        Value::Nil
    } else {
        raw
    }
}

fn expect_keymap_value(eval: &Evaluator, value: &Value) -> Result<Value, Flow> {
    if is_list_keymap(value) {
        return Ok(*value);
    }
    // Check if it's a symbol whose function cell is a keymap
    if let Some(name) = value.as_symbol_name() {
        if let Some(func) = eval.obarray.symbol_function(name).copied() {
            if is_list_keymap(&func) {
                return Ok(func);
            }
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("keymapp"), *value],
    ))
}

fn where_is_internal_keymaps(eval: &Evaluator, value: &Value) -> Result<Vec<Value>, Flow> {
    if is_list_keymap(value) {
        return Ok(vec![*value]);
    }

    if let Some(items) = list_to_vec(value) {
        let mut keymaps = Vec::with_capacity(items.len());
        for item in items {
            keymaps.push(expect_keymap_value(eval, &item)?);
        }
        return Ok(keymaps);
    }

    Ok(vec![expect_keymap_value(eval, value)?])
}

fn lookup_keymap_with_partial_value(keymap: &Value, events: &[KeyEvent]) -> Value {
    if events.is_empty() {
        return *keymap;
    }

    // Convert KeyEvent to emacs event Values
    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();
    list_keymap_lookup_seq(keymap, &emacs_events)
}

/// Same as above but already has emacs events as Values.
fn lookup_keymap_with_partial(keymap: &Value, emacs_events: &[Value]) -> Value {
    if emacs_events.is_empty() {
        return *keymap;
    }
    list_keymap_lookup_seq(keymap, emacs_events)
}

fn binding_matches_definition(binding: &Value, definition: &Value) -> bool {
    if binding.is_nil() {
        return false;
    }
    // If binding is a keymap (prefix), it doesn't match a command definition
    if is_list_keymap(binding) {
        return false;
    }
    // Symbol comparison
    if let (Some(bname), Some(dname)) = (binding.as_symbol_name(), definition.as_symbol_name()) {
        return bname == dname;
    }
    // Subr comparison
    if let (Value::Subr(bid), Value::Subr(did)) = (binding, definition) {
        return bid == did;
    }
    // Check if binding is a symbol matching a Subr definition name
    if let Some(bname) = binding.as_symbol_name() {
        if let Value::Subr(id) = definition {
            return bname == resolve_sym(*id);
        }
    }
    binding == definition
}

fn collect_where_is_sequences_value(
    keymap: &Value,
    definition: &Value,
    prefix: &mut Vec<Value>,
    out: &mut Vec<Vec<Value>>,
    first_only: bool,
    depth: usize,
) -> bool {
    if depth > 50 {
        return false; // Prevent infinite recursion in circular keymaps
    }

    // Collect all bindings from the alist portion
    let mut bindings: Vec<(Value, Value)> = Vec::new();
    list_keymap_for_each_binding(keymap, |event, binding| {
        bindings.push((event, binding));
    });

    // Sort by event description for consistent ordering
    bindings.sort_by(|(a, _), (b, _)| {
        let a_str = if let Some(name) = a.as_symbol_name() {
            name.to_string()
        } else if let Some(ke) = super::keymap::emacs_event_to_key_event(a) {
            format_key_event(&ke)
        } else {
            format!("{}", a)
        };
        let b_str = if let Some(name) = b.as_symbol_name() {
            name.to_string()
        } else if let Some(ke) = super::keymap::emacs_event_to_key_event(b) {
            format_key_event(&ke)
        } else {
            format!("{}", b)
        };
        a_str.cmp(&b_str)
    });

    for (event, binding) in bindings {
        prefix.push(event);
        if is_list_keymap(&binding) {
            if collect_where_is_sequences_value(
                &binding,
                definition,
                prefix,
                out,
                first_only,
                depth + 1,
            ) {
                prefix.pop();
                return true;
            }
        } else if binding_matches_definition(&binding, definition) {
            out.push(prefix.clone());
            if first_only {
                prefix.pop();
                return true;
            }
        }
        prefix.pop();
    }

    // Check parent keymap
    let parent = super::keymap::list_keymap_parent(keymap);
    if is_list_keymap(&parent) {
        if collect_where_is_sequences_value(&parent, definition, prefix, out, first_only, depth + 1)
        {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Thing-at-point extraction helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "interactive_test.rs"]
mod tests;
