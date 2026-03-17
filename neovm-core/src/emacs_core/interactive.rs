//! Interactive command system.
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

use std::collections::{HashMap, HashSet};

use super::error::{EvalResult, Flow, signal};
use super::eval::{Evaluator, quote_to_value, value_to_expr};
use super::expr::Expr;
use super::intern::{intern, resolve_sym};
use super::keymap::{
    KeyEvent, expand_meta_prefix_char_events_in_obarray, format_key_event, format_key_sequence,
    is_list_keymap, key_event_to_emacs_event, list_keymap_for_each_binding, list_keymap_lookup_one,
    list_keymap_lookup_seq, make_list_keymap, make_sparse_list_keymap,
};
use super::mode::{MajorMode, MinorMode};
use super::symbol::Obarray;
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

fn interactive_form_from_spec_value(spec: Value) -> Value {
    Value::list(vec![Value::symbol("interactive"), spec])
}

fn interactive_form_from_string_spec(code: &str) -> Value {
    interactive_form_from_spec_value(if code.is_empty() {
        Value::Nil
    } else {
        Value::string(code)
    })
}

pub(crate) fn registry_interactive_form(
    registry: &InteractiveRegistry,
    name: &str,
) -> Option<Value> {
    registry
        .get_spec(name)
        .map(|spec| interactive_form_from_string_spec(&spec.code))
}

pub(crate) fn builtin_subr_interactive_form(name: &str) -> Option<Value> {
    match name {
        "ignore" => Some(interactive_form_from_spec_value(Value::Nil)),
        "forward-char"
        | "backward-char"
        | "beginning-of-line"
        | "end-of-line"
        | "move-beginning-of-line"
        | "move-end-of-line"
        | "forward-word"
        | "backward-word" => Some(interactive_form_from_string_spec("^p")),
        "forward-sexp" | "backward-sexp" => Some(interactive_form_from_string_spec("^p\nd")),
        "delete-char" => Some(interactive_form_from_string_spec("p\nP")),
        "kill-word" | "backward-kill-word" | "upcase-word" | "downcase-word"
        | "capitalize-word" => Some(interactive_form_from_string_spec("p")),
        "transpose-lines"
        | "transpose-words"
        | "transpose-sentences"
        | "transpose-paragraphs"
        | "open-line"
        | "just-one-space" => Some(interactive_form_from_string_spec("*p")),
        "transpose-sexps" => Some(interactive_form_from_string_spec("*p\nd")),
        "delete-horizontal-space" => Some(interactive_form_from_string_spec("*P")),
        "scroll-up-command" | "scroll-down-command" => {
            Some(interactive_form_from_string_spec("^P"))
        }
        "set-mark-command" => Some(interactive_form_from_string_spec("P")),
        "move-to-column" => Some(interactive_form_from_string_spec("NMove to column: ")),
        "goto-char" => Some(interactive_form_from_spec_value(Value::list(vec![
            Value::symbol("goto-char--read-natnum-interactive"),
            Value::string("Go to char: "),
        ]))),
        "self-insert-command" => Some(interactive_form_from_spec_value(Value::list(vec![
            Value::symbol("list"),
            Value::list(vec![
                Value::symbol("prefix-numeric-value"),
                Value::symbol("current-prefix-arg"),
            ]),
            Value::symbol("last-command-event"),
        ]))),
        _ => None,
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
    let plan = {
        let obarray = &eval.obarray;
        let interactive = &eval.interactive;
        let read_command_keys = eval.read_command_keys();
        plan_call_interactively_in_state(obarray, interactive, read_command_keys, &args)?
    };
    finish_call_interactively_in_eval(eval, plan)
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
    builtin_commandp_in_state(&eval.obarray, &eval.interactive, &args)
}

pub(crate) fn builtin_commandp_in_state(
    obarray: &Obarray,
    interactive: &InteractiveRegistry,
    args: &[Value],
) -> EvalResult {
    expect_min_args("commandp", &args, 1)?;
    expect_max_args("commandp", &args, 2)?;
    let is_command = command_designator_p_in_state(
        obarray,
        interactive,
        &args[0],
        args.get(1).is_some_and(|value| !value.is_nil()),
    );
    Ok(Value::bool(is_command))
}

/// `(command-modes COMMAND)` -- return COMMAND's mode list.
///
/// Current compatibility behavior returns nil.
pub(crate) fn builtin_command_modes(args: Vec<Value>) -> EvalResult {
    expect_args("command-modes", &args, 1)?;
    Ok(Value::Nil)
}

fn command_modes_from_expr_body(body: &[Expr]) -> Option<Value> {
    let body_index = lambda_body_metadata_end(body);
    for expr in &body[body_index..] {
        let Expr::List(items) = expr else {
            continue;
        };
        let Some(Expr::Symbol(head_id)) = items.first() else {
            continue;
        };
        if resolve_sym(*head_id) != "interactive" {
            continue;
        }
        let modes = items.iter().skip(2).map(quote_to_value).collect::<Vec<_>>();
        return Some(if modes.is_empty() {
            Value::Nil
        } else {
            Value::list(modes)
        });
    }
    None
}

fn unquote_command_modes_value(value: Value) -> Value {
    let Some(items) = value_list_to_vec(&value) else {
        return value;
    };
    if items.len() == 2 && items[0].as_symbol_name() == Some("quote") {
        items[1]
    } else {
        value
    }
}

fn command_modes_from_quoted_interactive_form(form: &Value) -> Result<Option<Value>, Flow> {
    let Value::Cons(cell) = form else {
        return Ok(None);
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("interactive") {
        return Ok(None);
    }

    match pair.cdr {
        Value::Nil => Ok(Some(Value::Nil)),
        Value::Cons(arg_cell) => {
            let arg_pair = read_cons(arg_cell);
            Ok(Some(arg_pair.cdr))
        }
        tail => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), tail],
        )),
    }
}

fn command_modes_from_quoted_lambda(value: &Value) -> Result<Option<Value>, Flow> {
    let Some(items) = value_list_to_vec(value) else {
        return Ok(None);
    };
    if items.first().and_then(Value::as_symbol_name) != Some("lambda") {
        return Ok(None);
    }

    let mut body_index = 2;
    if matches!(items.get(body_index), Some(Value::Str(_))) {
        body_index += 1;
    }
    while items.get(body_index).is_some_and(value_is_declare_form) {
        body_index += 1;
    }

    for form in &items[body_index..] {
        if let Some(modes) = command_modes_from_quoted_interactive_form(form)? {
            return Ok(Some(modes));
        }
    }

    Ok(None)
}

pub(crate) fn builtin_command_modes_in_state(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("command-modes", args, 1)?;
    let command = args[0];
    let mut function = command;

    if let Some(mut current) = crate::emacs_core::builtins::symbols::symbol_id(&command) {
        let Some((_, indirect_function)) =
            crate::emacs_core::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
                obarray, current,
            )
        else {
            return Ok(Value::Nil);
        };
        if indirect_function.is_nil() {
            return Ok(Value::Nil);
        }

        loop {
            if let Some(modes) = obarray
                .get_property_id(current, intern("command-modes"))
                .copied()
                .filter(|value| !value.is_nil())
            {
                return Ok(modes);
            }
            let Some(next_function) =
                crate::emacs_core::builtins::symbols::symbol_function_cell_in_obarray(
                    obarray, current,
                )
            else {
                return Ok(Value::Nil);
            };
            function = next_function;
            let Some(next_symbol) = crate::emacs_core::builtins::symbols::symbol_id(&function)
            else {
                break;
            };
            current = next_symbol;
        }
    }

    match function {
        Value::Subr(_) => Ok(Value::Nil),
        Value::Lambda(id) | Value::Macro(id) => {
            let body = with_heap(|h| h.get_lambda(id).body.clone());
            Ok(command_modes_from_expr_body(&body).unwrap_or(Value::Nil))
        }
        Value::ByteCode(id) => {
            let interactive = with_heap(|h| h.get_bytecode(id).interactive);
            let Some(Value::Vector(vec_id)) = interactive else {
                return Ok(Value::Nil);
            };
            Ok(with_heap(|h| {
                if h.vector_len(vec_id) > 1 {
                    unquote_command_modes_value(h.vector_ref(vec_id, 1))
                } else {
                    Value::Nil
                }
            }))
        }
        Value::Cons(_) if super::autoload::is_autoload_value(&function) => {
            let Some(items) = value_list_to_vec(&function) else {
                return Ok(Value::Nil);
            };
            Ok(match items.get(3).copied() {
                Some(Value::Cons(_)) => items[3],
                _ => Value::Nil,
            })
        }
        Value::Cons(_) => Ok(command_modes_from_quoted_lambda(&function)?.unwrap_or(Value::Nil)),
        _ => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_command_modes_eval(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_command_modes_in_state(&eval.obarray, &args)
}

/// `(command-remapping COMMAND &optional POSITION KEYMAP)` -- return remapped
/// command for COMMAND.
///
/// Respects local/global keymaps when KEYMAP is omitted or nil.
pub(crate) fn builtin_command_remapping(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_command_remapping_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        eval.current_local_map,
        args,
    )
}

pub(crate) fn builtin_command_remapping_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    current_local_map: Value,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("command-remapping", &args, 1)?;
    expect_max_args("command-remapping", &args, 3)?;
    if let Some(keymap) = args.get(2) {
        if !keymap.is_nil() && !command_remapping_keymap_arg_valid(keymap) {
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
        interactive_validate_integer_position_arg_in_buffers(buffers, position)?;
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
                return Ok(command_remapping_lookup_in_active_keymaps_in_state(
                    obarray,
                    dynamic,
                    current_local_map,
                    &command_name,
                )
                .unwrap_or(Value::Nil));
            }
            _ => {
                // Not a valid keymap
                return Ok(Value::Nil);
            }
        }
    }
    Ok(command_remapping_lookup_in_active_keymaps_in_state(
        obarray,
        dynamic,
        current_local_map,
        &command_name,
    )
    .unwrap_or(Value::Nil))
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
            | "forward-line"
            | "forward-sexp"
            | "forward-word"
            | "garbage-collect"
            | "getenv"
            | "gui-set-selection"
            | "goto-char"
            | "increment-register"
            | "indent-rigidly"
            | "indent-to"
            | "insert-register"
            | "iconify-frame"
            | "isearch-backward"
            | "isearch-forward"
            | "kill-buffer"
            | "kill-emacs"
            | "kill-local-variable"
            | "lower-frame"
            | "lossage-size"
            | "malloc-info"
            | "malloc-trim"
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
            | "number-to-register"
            | "open-dribble-file"
            | "open-termscript"
            | "point-to-register"
            | "pop-to-buffer"
            | "posix-search-backward"
            | "posix-search-forward"
            | "raise-frame"
            | "recenter"
            | "redirect-debugging-output"
            | "re-search-backward"
            | "re-search-forward"
            | "recursive-edit"
            | "rename-buffer"
            | "rename-file"
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
            | "start-kbd-macro"
            | "suspend-emacs"
            | "top-level"
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
    resolve_function_designator_symbol_in_state(&eval.obarray, name)
}

fn resolve_function_designator_symbol_in_state(
    obarray: &Obarray,
    name: &str,
) -> Option<(String, Value)> {
    crate::emacs_core::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
        obarray,
        intern(name),
    )
    .map(|(resolved, value)| (resolve_sym(resolved).to_string(), value))
}

fn command_object_p_in_state(
    interactive: &InteractiveRegistry,
    resolved_name: Option<&str>,
    value: &Value,
    for_call_interactively: bool,
) -> bool {
    if let Some(name) = resolved_name {
        if interactive.is_interactive(name) || builtin_command_name(name) {
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
        Value::ByteCode(_) => value
            .get_bytecode_data()
            .is_some_and(|bc| bc.interactive.is_some()),
        Value::Cons(_) => quoted_lambda_has_interactive_form(value),
        Value::Subr(id) => {
            let name = resolve_sym(*id);
            interactive.is_interactive(name) || builtin_command_name(name)
        }
        Value::Str(_) | Value::Vector(_) => !for_call_interactively,
        _ => false,
    }
}

fn command_designator_p_in_state(
    obarray: &Obarray,
    interactive: &InteractiveRegistry,
    designator: &Value,
    for_call_interactively: bool,
) -> bool {
    if let Some(name) = designator.as_symbol_name() {
        if obarray.is_function_unbound(name) {
            return false;
        }
        if let Some((resolved_name, resolved_value)) =
            resolve_function_designator_symbol_in_state(obarray, name)
        {
            return command_object_p_in_state(
                interactive,
                Some(&resolved_name),
                &resolved_value,
                for_call_interactively,
            );
        }
        return interactive.is_interactive(name) || builtin_command_name(name);
    }
    command_object_p_in_state(interactive, None, designator, for_call_interactively)
}

fn command_object_p(eval: &Evaluator, resolved_name: Option<&str>, value: &Value) -> bool {
    command_object_p_in_state(&eval.interactive, resolved_name, value, false)
}

fn command_designator_p(eval: &Evaluator, designator: &Value) -> bool {
    command_designator_p_in_state(&eval.obarray, &eval.interactive, designator, false)
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
    pending_up_event: Option<Value>,
}

impl InteractiveInvocationContext {
    fn from_keys_arg(eval: &Evaluator, keys: Option<&Value>) -> Self {
        Self::from_keys_arg_in_state(eval.read_command_keys(), keys)
    }

    fn from_keys_arg_in_state(read_command_keys: &[Value], keys: Option<&Value>) -> Self {
        let mut context = Self::default();
        if let Some(Value::Vector(values)) = keys {
            let values = with_heap(|h| h.get_vector(*values).clone());
            if !values.is_empty() {
                context.command_keys = values.clone();
                context.has_command_keys_context = true;
                return context;
            }
        }
        if !read_command_keys.is_empty() {
            context.command_keys = read_command_keys.to_vec();
            context.has_command_keys_context = true;
        }
        context
    }
}

fn interactive_event_symbol_name(event: &Value) -> Option<&'static str> {
    match event {
        Value::Symbol(id) => Some(resolve_sym(*id)),
        Value::Cons(cell) => match read_cons(*cell).car {
            Value::Symbol(id) => Some(resolve_sym(id)),
            _ => None,
        },
        _ => None,
    }
}

fn interactive_strip_event_modifier_prefixes(mut name: &str) -> &str {
    loop {
        if let Some(rest) = name.strip_prefix("C-") {
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("M-") {
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("S-") {
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("s-") {
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("H-") {
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("A-") {
            name = rest;
            continue;
        }
        break;
    }
    name
}

fn interactive_event_is_down_event(event: &Value) -> bool {
    let Some(name) = interactive_event_symbol_name(event) else {
        return false;
    };
    interactive_strip_event_modifier_prefixes(name).starts_with("down-")
}

fn interactive_last_key_sequence_event(sequence: &Value) -> Option<Value> {
    match sequence {
        Value::Vector(id) => super::value::with_heap(|h| h.get_vector(*id).last().copied()),
        _ => None,
    }
}

fn interactive_capture_up_event_in_eval(
    eval: &mut Evaluator,
    sequence: &Value,
    context: &mut InteractiveInvocationContext,
) -> Result<(), Flow> {
    context.pending_up_event = None;
    if interactive_last_key_sequence_event(sequence)
        .is_some_and(|event| interactive_event_is_down_event(&event))
    {
        let up_event = super::lread::builtin_read_event(eval, vec![])?;
        if !up_event.is_nil() {
            context.pending_up_event = Some(up_event);
        }
    }
    Ok(())
}

fn interactive_capture_up_event_in_vm_batch_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    sequence: &Value,
    context: &mut InteractiveInvocationContext,
) -> Result<(), Flow> {
    context.pending_up_event = None;
    if interactive_last_key_sequence_event(sequence)
        .is_some_and(|event| interactive_event_is_down_event(&event))
    {
        if let Some(up_event) = super::lread::builtin_read_event_in_runtime(shared, &[])? {
            if !up_event.is_nil() {
                context.pending_up_event = Some(up_event);
            }
        }
    }
    Ok(())
}

fn interactive_u_arg(context: &mut InteractiveInvocationContext) -> Value {
    context
        .pending_up_event
        .take()
        .map(|event| Value::vector(vec![event]))
        .unwrap_or(Value::Nil)
}

fn dynamic_or_global_symbol_value(eval: &Evaluator, name: &str) -> Option<Value> {
    dynamic_or_global_symbol_value_in_state(&eval.obarray, eval.dynamic.as_slice(), name)
}

fn dynamic_or_global_symbol_value_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(v) = frame.get(&name_id) {
            return Some(*v);
        }
    }
    obarray.symbol_value(name).cloned()
}

fn dynamic_buffer_or_global_symbol_value(
    eval: &Evaluator,
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    dynamic_buffer_or_global_symbol_value_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        buf,
        name,
    )
}

fn dynamic_buffer_or_global_symbol_value_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(v) = frame.get(&name_id) {
            return Some(*v);
        }
    }
    if let Some(v) = buf.get_buffer_local(name) {
        return Some(*v);
    }
    obarray.symbol_value(name).cloned()
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
    interactive_prefix_raw_arg_in_state(&eval.obarray, eval.dynamic.as_slice(), kind)
}

fn interactive_prefix_raw_arg_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    kind: CommandInvocationKind,
) -> Value {
    let symbol = match kind {
        CommandInvocationKind::CallInteractively => "current-prefix-arg",
        CommandInvocationKind::CommandExecute => "prefix-arg",
    };
    dynamic_or_global_symbol_value_in_state(obarray, dynamic, symbol).unwrap_or(Value::Nil)
}

fn interactive_prefix_numeric_arg(eval: &Evaluator, kind: CommandInvocationKind) -> Value {
    let raw = interactive_prefix_raw_arg(eval, kind);
    Value::Int(prefix_numeric_value(&raw))
}

fn interactive_prefix_numeric_arg_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    kind: CommandInvocationKind,
) -> Value {
    let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic, kind);
    Value::Int(prefix_numeric_value(&raw))
}

fn interactive_region_args(
    eval: &Evaluator,
    missing_mark_signal: &str,
) -> Result<Vec<Value>, Flow> {
    interactive_region_args_in_buffers(&eval.buffers, missing_mark_signal)
}

fn interactive_region_args_in_buffers(
    buffers: &crate::buffer::BufferManager,
    missing_mark_signal: &str,
) -> Result<Vec<Value>, Flow> {
    let buf = buffers
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
    interactive_point_arg_in_buffers(&eval.buffers)
}

fn interactive_point_arg_in_buffers(buffers: &crate::buffer::BufferManager) -> Result<Value, Flow> {
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_char = buf.point_char() as i64 + 1;
    Ok(Value::Int(point_char))
}

fn interactive_mark_arg(eval: &Evaluator) -> Result<Value, Flow> {
    interactive_mark_arg_in_buffers(&eval.buffers)
}

fn interactive_mark_arg_in_buffers(buffers: &crate::buffer::BufferManager) -> Result<Value, Flow> {
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.mark()
        .ok_or_else(|| signal("error", vec![Value::string("The mark is not set now")]))?;
    let mark_char = buf.mark_char().expect("mark byte/char stay in sync") as i64 + 1;
    Ok(Value::Int(mark_char))
}

fn interactive_string_code_returns_no_args_without_eval(code: &str) -> bool {
    let parsed = parse_interactive_code_entries(code);
    parsed.prefix_flags.is_empty() && parsed.entries.is_empty()
}

fn interactive_last_input_event_with_parameters_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
) -> Option<Value> {
    let event = dynamic_or_global_symbol_value_in_state(obarray, dynamic, "last-input-event")?;
    interactive_event_with_parameters_p(&event).then_some(event)
}

fn interactive_next_event_with_parameters_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    context: &mut InteractiveInvocationContext,
) -> Option<Value> {
    if context.has_command_keys_context {
        return interactive_next_event_with_parameters_from_keys(context);
    }
    interactive_last_input_event_with_parameters_in_state(obarray, dynamic)
}

fn interactive_args_from_string_code_in_state(
    obarray: &mut Obarray,
    dynamic: &mut Vec<OrderedRuntimeBindingMap>,
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    code: &str,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
) -> Result<Option<Vec<Value>>, Flow> {
    let parsed = parse_interactive_code_entries(code);
    interactive_apply_prefix_flags_in_state(
        obarray,
        dynamic.as_mut_slice(),
        buffers,
        custom,
        &parsed.prefix_flags,
    )?;
    if parsed.entries.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut args = Vec::new();
    for (letter, _prompt) in parsed.entries {
        match letter {
            'd' => args.push(interactive_point_arg_in_buffers(buffers)?),
            'e' => {
                if let Some(event) =
                    interactive_next_event_with_parameters_in_state(obarray, dynamic, context)
                {
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
            'i' => args.push(Value::Nil),
            'm' => args.push(interactive_mark_arg_in_buffers(buffers)?),
            'N' => {
                let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic.as_slice(), kind);
                if raw.is_nil() {
                    return Ok(None);
                }
                args.push(Value::Int(prefix_numeric_value(&raw)));
            }
            'p' => args.push(interactive_prefix_numeric_arg_in_state(
                obarray,
                dynamic.as_slice(),
                kind,
            )),
            'P' => args.push(interactive_prefix_raw_arg_in_state(
                obarray,
                dynamic.as_slice(),
                kind,
            )),
            'r' => args.extend(interactive_region_args_in_buffers(buffers, "error")?),
            'U' => args.push(Value::Nil),
            'Z' => {
                let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic.as_slice(), kind);
                if raw.is_nil() {
                    args.push(Value::Nil);
                } else {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
    }

    Ok(Some(args))
}

fn interactive_args_from_string_code_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    code: &str,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
    vm_gc_roots: &[Value],
) -> Result<Option<Vec<Value>>, Flow> {
    let parsed = parse_interactive_code_entries(code);
    interactive_apply_prefix_flags_in_state(
        &mut *shared.obarray,
        shared.dynamic.as_mut_slice(),
        shared.buffers,
        &*shared.custom,
        &parsed.prefix_flags,
    )?;
    if parsed.entries.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut args = Vec::new();
    for (letter, prompt) in parsed.entries {
        match letter {
            'a' | 'C' => {
                let letter_args = [Value::string(prompt)];
                super::minibuffer::builtin_read_command_in_runtime(shared, &letter_args)?;
                args.push(super::minibuffer::finish_read_command_with_minibuffer(
                    &letter_args,
                    |minibuffer_args| {
                        super::reader::finish_read_from_minibuffer_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            minibuffer_args,
                        )
                    },
                )?);
            }
            'b' => {
                let letter_args = [Value::string(prompt), Value::Nil, Value::True];
                super::minibuffer::builtin_read_buffer_in_runtime(shared, &letter_args)?;
                let completing_args = {
                    super::minibuffer::read_buffer_completing_args(&*shared.buffers, &letter_args)
                };
                args.push(super::reader::finish_completing_read_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &completing_args,
                )?);
            }
            'B' => {
                let letter_args = [Value::string(prompt), Value::Nil, Value::Nil];
                super::minibuffer::builtin_read_buffer_in_runtime(shared, &letter_args)?;
                let completing_args = {
                    super::minibuffer::read_buffer_completing_args(&*shared.buffers, &letter_args)
                };
                args.push(super::reader::finish_completing_read_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &completing_args,
                )?);
            }
            'c' => {
                let letter_args = [Value::string(prompt)];
                let arg = if let Some(arg) =
                    super::reader::builtin_read_char_in_runtime(shared, &letter_args)?
                {
                    arg
                } else {
                    super::reader::finish_read_char_interactive_in_runtime(shared, &letter_args)?
                };
                args.push(arg);
            }
            'd' => args.push(interactive_point_arg_in_buffers(shared.buffers)?),
            'D' => {
                let letter_args = [Value::string(prompt)];
                args.push(super::minibuffer::finish_read_directory_name_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &letter_args,
                )?);
            }
            'e' => {
                if let Some(event) = interactive_next_event_with_parameters_in_state(
                    &mut *shared.obarray,
                    shared.dynamic,
                    context,
                ) {
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
            'f' => {
                let letter_args = [Value::string(prompt), Value::Nil, Value::Nil, Value::True];
                args.push(super::minibuffer::finish_read_file_name_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &letter_args,
                )?);
            }
            'F' | 'G' => {
                let letter_args = [Value::string(prompt)];
                args.push(super::minibuffer::finish_read_file_name_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &letter_args,
                )?);
            }
            'i' => args.push(Value::Nil),
            'k' => {
                let letter_args = [Value::string(prompt)];
                let arg = if let Some(arg) =
                    super::reader::builtin_read_key_sequence_in_runtime(shared, &letter_args)?
                {
                    arg
                } else {
                    super::reader::finish_read_key_sequence_interactive_in_runtime(shared)?
                };
                interactive_capture_up_event_in_vm_batch_runtime(shared, &arg, context)?;
                args.push(arg);
            }
            'K' => {
                let letter_args = [Value::string(prompt)];
                let arg = if let Some(arg) =
                    super::reader::builtin_read_key_sequence_vector_in_runtime(
                        shared,
                        &letter_args,
                    )? {
                    arg
                } else {
                    super::reader::finish_read_key_sequence_vector_interactive_in_runtime(shared)?
                };
                interactive_capture_up_event_in_vm_batch_runtime(shared, &arg, context)?;
                args.push(arg);
            }
            'M' => {
                let letter_args = [
                    Value::string(prompt),
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::True,
                ];
                super::reader::builtin_read_string_in_runtime(shared, &letter_args)?;
                args.push(super::reader::finish_read_string_with_minibuffer(
                    &letter_args,
                    |minibuffer_args| {
                        super::reader::finish_read_from_minibuffer_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            minibuffer_args,
                        )
                    },
                )?);
            }
            'm' => args.push(interactive_mark_arg_in_buffers(shared.buffers)?),
            'N' => {
                let raw = interactive_prefix_raw_arg_in_state(
                    &*shared.obarray,
                    shared.dynamic.as_slice(),
                    kind,
                );
                if raw.is_nil() {
                    let letter_args = [Value::string(prompt)];
                    args.push(super::reader::finish_read_number_in_vm_runtime(
                        shared,
                        vm_gc_roots,
                        &letter_args,
                    )?);
                } else {
                    args.push(Value::Int(prefix_numeric_value(&raw)));
                }
            }
            'p' => args.push(interactive_prefix_numeric_arg_in_state(
                &*shared.obarray,
                shared.dynamic.as_slice(),
                kind,
            )),
            'P' => args.push(interactive_prefix_raw_arg_in_state(
                &*shared.obarray,
                shared.dynamic.as_slice(),
                kind,
            )),
            'r' => args.extend(interactive_region_args_in_buffers(shared.buffers, "error")?),
            'R' => {
                if interactive_use_region_p_in_vm_runtime(shared, vm_gc_roots)? {
                    args.extend(interactive_region_args_in_buffers(shared.buffers, "error")?);
                } else {
                    args.push(Value::Nil);
                    args.push(Value::Nil);
                }
            }
            'n' => {
                let letter_args = [Value::string(prompt)];
                args.push(super::reader::finish_read_number_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &letter_args,
                )?);
            }
            's' => {
                let letter_args = [Value::string(prompt)];
                super::reader::builtin_read_string_in_runtime(shared, &letter_args)?;
                args.push(super::reader::finish_read_string_with_minibuffer(
                    &letter_args,
                    |minibuffer_args| {
                        super::reader::finish_read_from_minibuffer_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            minibuffer_args,
                        )
                    },
                )?);
            }
            'S' => {
                let letter_args = [Value::string(prompt)];
                super::reader::builtin_read_string_in_runtime(shared, &letter_args)?;
                let sym_name = super::reader::finish_read_string_with_minibuffer(
                    &letter_args,
                    |minibuffer_args| {
                        super::reader::finish_read_from_minibuffer_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            minibuffer_args,
                        )
                    },
                )?;
                if let Some(name) = sym_name.as_str() {
                    args.push(Value::symbol(name));
                } else {
                    return Ok(None);
                }
            }
            'U' => args.push(interactive_u_arg(context)),
            'v' => {
                let letter_args = [Value::string(prompt)];
                super::minibuffer::builtin_read_variable_in_runtime(shared, &letter_args)?;
                args.push(super::minibuffer::finish_read_variable_with_minibuffer(
                    &letter_args,
                    |minibuffer_args| {
                        super::reader::finish_read_from_minibuffer_in_vm_runtime(
                            shared,
                            vm_gc_roots,
                            minibuffer_args,
                        )
                    },
                )?);
            }
            'z' => args.push(super::lread::builtin_read_coding_system(vec![
                Value::string(prompt),
            ])?),
            'Z' => {
                let raw = interactive_prefix_raw_arg_in_state(
                    &*shared.obarray,
                    shared.dynamic.as_slice(),
                    kind,
                );
                if raw.is_nil() {
                    args.push(Value::Nil);
                } else {
                    args.push(interactive_read_coding_system_optional_arg(prompt)?);
                }
            }
            _ => return Ok(None),
        }
    }

    Ok(Some(args))
}

fn default_command_execute_args_in_state(
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    name: &str,
) -> Result<Vec<Value>, Flow> {
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
        | "scroll-down-command" => Ok(vec![Value::Int(1)]),
        "kill-region" => interactive_region_args_in_buffers(buffers, "user-error"),
        "kill-ring-save" => interactive_region_args_in_buffers(buffers, "error"),
        "copy-region-as-kill" => interactive_region_args_in_buffers(buffers, "error"),
        "set-mark-command" => Ok(vec![Value::Nil]),
        "split-window-below" | "split-window-right" => {
            let win = frames
                .selected_frame()
                .map(|f| Value::Window(f.selected_window.0))
                .unwrap_or(Value::Nil);
            Ok(vec![Value::Nil, win])
        }
        "capitalize-region" => interactive_region_args_in_buffers(buffers, "error"),
        "upcase-initials-region" => interactive_region_args_in_buffers(buffers, "error"),
        "upcase-region" | "downcase-region" => Err(signal(
            "args-out-of-range",
            vec![Value::string(""), Value::Int(0)],
        )),
        _ => Ok(Vec::new()),
    }
}

fn default_call_interactively_args_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    frames: &crate::window::FrameManager,
    name: &str,
) -> Result<Vec<Value>, Flow> {
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
        | "move-end-of-line" => Ok(vec![interactive_prefix_numeric_arg_in_state(
            obarray,
            dynamic,
            CommandInvocationKind::CallInteractively,
        )]),
        "set-mark-command" => Ok(vec![interactive_prefix_raw_arg_in_state(
            obarray,
            dynamic,
            CommandInvocationKind::CallInteractively,
        )]),
        "upcase-region" | "downcase-region" | "capitalize-region" => {
            interactive_region_args_in_buffers(buffers, "error")
        }
        _ => default_command_execute_args_in_state(buffers, frames, name),
    }
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

fn interactive_use_region_p_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
) -> Result<bool, Flow> {
    shared
        .with_parent_evaluator_vm_roots(vm_gc_roots, &[], |eval| {
            eval.apply(Value::symbol("use-region-p"), vec![])
        })
        .map(|value| value.is_truthy())
}

fn interactive_buffer_read_only_active(eval: &Evaluator, buf: &crate::buffer::Buffer) -> bool {
    interactive_buffer_read_only_active_in_state(&eval.obarray, eval.dynamic.as_slice(), buf)
}

fn interactive_buffer_read_only_active_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &crate::buffer::Buffer,
) -> bool {
    if buf.read_only {
        return true;
    }
    dynamic_buffer_or_global_symbol_value_in_state(obarray, dynamic, buf, "buffer-read-only")
        .is_some_and(|v| v.is_truthy())
}

fn interactive_require_writable_current_buffer(eval: &Evaluator) -> Result<(), Flow> {
    interactive_require_writable_current_buffer_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
    )
}

fn interactive_require_writable_current_buffer_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
) -> Result<(), Flow> {
    let Some(buf) = buffers.current_buffer() else {
        return Ok(());
    };
    if dynamic_buffer_or_global_symbol_value_in_state(obarray, dynamic, buf, "inhibit-read-only")
        .is_some_and(|v| v.is_truthy())
    {
        return Ok(());
    }
    if interactive_buffer_read_only_active_in_state(obarray, dynamic, buf) {
        return Err(signal("buffer-read-only", vec![Value::string(&buf.name)]));
    }
    Ok(())
}

fn interactive_apply_shift_selection_prefix(eval: &mut Evaluator) {
    interactive_apply_shift_selection_prefix_in_state(
        &mut eval.obarray,
        eval.dynamic.as_mut_slice(),
        &mut eval.buffers,
        &eval.custom,
    );
}

fn interactive_apply_shift_selection_prefix_in_state(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
) {
    let shifted = dynamic_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        "this-command-keys-shift-translated",
    )
    .is_some_and(|v| v.is_truthy());
    let shift_select_mode =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "shift-select-mode")
            .is_some_and(|v| v.is_truthy());
    if !shifted || !shift_select_mode {
        return;
    }

    let mut mark_activated = false;
    if let Some(current_id) = buffers.current_buffer_id() {
        let point = buffers.get(current_id).map(|buf| buf.point()).unwrap_or(0);
        let _ = buffers.set_buffer_mark(current_id, point);
        let _ = buffers.set_buffer_local_property(current_id, "mark-active", Value::True);
        mark_activated = true;
    }
    if mark_activated {
        let _ = super::eval::set_runtime_binding_in_state(
            obarray,
            dynamic,
            buffers,
            custom,
            intern("mark-active"),
            Value::True,
        );
    }
}

fn interactive_apply_prefix_flags(eval: &mut Evaluator, prefix_flags: &[char]) -> Result<(), Flow> {
    interactive_apply_prefix_flags_in_state(
        &mut eval.obarray,
        eval.dynamic.as_mut_slice(),
        &mut eval.buffers,
        &eval.custom,
        prefix_flags,
    )
}

fn interactive_apply_prefix_flags_in_state(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    prefix_flags: &[char],
) -> Result<(), Flow> {
    for prefix_flag in prefix_flags {
        match prefix_flag {
            '*' => interactive_require_writable_current_buffer_in_state(obarray, dynamic, buffers)?,
            '@' => {
                // Selecting the window from the first mouse event requires command-loop
                // event context; current batch paths have no such events yet.
            }
            '^' => {
                interactive_apply_shift_selection_prefix_in_state(obarray, dynamic, buffers, custom)
            }
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
            'k' => {
                let arg =
                    super::reader::builtin_read_key_sequence(eval, vec![Value::string(prompt)])?;
                interactive_capture_up_event_in_eval(eval, &arg, context)?;
                args.push(arg);
            }
            'K' => {
                let arg = super::reader::builtin_read_key_sequence_vector(
                    eval,
                    vec![Value::string(prompt)],
                )?;
                interactive_capture_up_event_in_eval(eval, &arg, context)?;
                args.push(arg);
            }
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
            'R' => {
                let use_region = eval
                    .apply(Value::symbol("use-region-p"), vec![])?
                    .is_truthy();
                if use_region {
                    args.extend(interactive_region_args(eval, "error")?);
                } else {
                    args.push(Value::Nil);
                    args.push(Value::Nil);
                }
            }
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
            'U' => args.push(interactive_u_arg(context)),
            'v' => args.push(super::minibuffer::builtin_read_variable(
                eval,
                vec![Value::string(prompt)],
            )?),
            'z' => args.push(super::lread::builtin_read_coding_system(vec![
                Value::string(prompt),
            ])?),
            'Z' => {
                let raw = interactive_prefix_raw_arg(eval, kind);
                if raw.is_nil() {
                    args.push(Value::Nil);
                } else {
                    args.push(interactive_read_coding_system_optional_arg(prompt)?);
                }
            }
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

fn eval_interactive_form_expr_in_vm_runtime(
    shared: &super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    form: &Expr,
) -> Result<Vec<Value>, Flow> {
    let value = super::eval::with_parent_evaluator_vm_roots_ptr(
        shared.parent_eval_ptr(),
        vm_gc_roots,
        &[],
        move |eval| eval.eval(form),
    )?;
    interactive_form_value_to_args(value)
}

fn eval_interactive_form_value_in_vm_runtime(
    shared: &super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    form: Value,
) -> Result<Vec<Value>, Flow> {
    let value = super::eval::with_parent_evaluator_vm_roots_ptr(
        shared.parent_eval_ptr(),
        vm_gc_roots,
        &[form],
        move |eval| eval.eval_value(&form),
    )?;
    interactive_form_value_to_args(value)
}

pub(crate) fn callable_form_needs_instantiation(value: &Value) -> bool {
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    matches!(
        items.first().and_then(Value::as_symbol_name),
        Some("lambda" | "closure")
    )
}

fn normalize_command_callable(eval: &mut Evaluator, value: Value) -> Result<Value, Flow> {
    if callable_form_needs_instantiation(&value) {
        return eval.eval_value(&value);
    }
    Ok(value)
}

fn default_command_execute_args(eval: &Evaluator, name: &str) -> Result<Vec<Value>, Flow> {
    default_command_execute_args_in_state(&eval.buffers, &eval.frames, name)
}

fn default_call_interactively_args(eval: &Evaluator, name: &str) -> Result<Vec<Value>, Flow> {
    default_call_interactively_args_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        &eval.frames,
        name,
    )
}

fn resolve_command_target(eval: &Evaluator, designator: &Value) -> Option<(String, Value)> {
    resolve_command_target_in_state(&eval.obarray, designator)
}

fn resolve_command_target_in_state(
    obarray: &Obarray,
    designator: &Value,
) -> Option<(String, Value)> {
    if let Some(name) = designator.as_symbol_name() {
        if let Some((resolved_name, value)) =
            resolve_function_designator_symbol_in_state(obarray, name)
        {
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

pub(crate) struct CallInteractivelyPlan {
    resolved_name: String,
    pub(crate) func: Value,
    context: InteractiveInvocationContext,
}

pub(crate) fn plan_call_interactively_in_state(
    obarray: &Obarray,
    interactive: &InteractiveRegistry,
    read_command_keys: &[Value],
    args: &[Value],
) -> Result<CallInteractivelyPlan, Flow> {
    expect_min_args("call-interactively", args, 1)?;
    expect_max_args("call-interactively", args, 3)?;
    expect_optional_command_keys_vector(args.get(2))?;

    let func_val = args[0];
    if !command_designator_p_in_state(obarray, interactive, &func_val, false) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("commandp"), func_val],
        ));
    }
    let Some((resolved_name, func)) = resolve_command_target_in_state(obarray, &func_val) else {
        return Err(signal("void-function", vec![func_val]));
    };
    let context =
        InteractiveInvocationContext::from_keys_arg_in_state(read_command_keys, args.get(2));
    Ok(CallInteractivelyPlan {
        resolved_name,
        func,
        context,
    })
}

pub(crate) fn finish_call_interactively_in_eval(
    eval: &mut Evaluator,
    mut plan: CallInteractivelyPlan,
) -> EvalResult {
    let (func, call_args) = resolve_call_interactively_target_and_args_in_eval(eval, &mut plan)?;

    eval.interactive.push_interactive_call(true);
    let result = eval.apply(func, call_args);
    eval.interactive.pop_interactive_call();
    result
}

pub(crate) fn resolve_call_interactively_target_and_args_in_eval(
    eval: &mut Evaluator,
    plan: &mut CallInteractivelyPlan,
) -> Result<(Value, Vec<Value>), Flow> {
    let func = normalize_command_callable(eval, plan.func)?;
    let call_args = resolve_interactive_invocation_args(
        eval,
        &plan.resolved_name,
        &func,
        CommandInvocationKind::CallInteractively,
        &mut plan.context,
    )?;
    Ok((func, call_args))
}

pub(crate) fn resolve_call_interactively_target_and_args_in_state(
    obarray: &mut Obarray,
    dynamic: &mut Vec<OrderedRuntimeBindingMap>,
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    frames: &crate::window::FrameManager,
    interactive: &InteractiveRegistry,
    plan: &mut CallInteractivelyPlan,
) -> Result<Option<(Value, Vec<Value>)>, Flow> {
    let func = plan.func;
    if let Some(code) = interactive
        .get_spec(&plan.resolved_name)
        .map(|spec| spec.code.as_str())
    {
        if let Some(args) = interactive_args_from_string_code_in_state(
            obarray,
            dynamic,
            buffers,
            custom,
            code,
            CommandInvocationKind::CallInteractively,
            &mut plan.context,
        )? {
            return Ok(Some((func, args)));
        }
        return Ok(None);
    }

    if let Some(lambda) = func.get_lambda_data()
        && let Some(spec) = parsed_interactive_spec_from_lambda(lambda)
    {
        return match spec {
            ParsedInteractiveSpec::NoArgs => Ok(Some((func, Vec::new()))),
            ParsedInteractiveSpec::StringCode(code) => interactive_args_from_string_code_in_state(
                obarray,
                dynamic,
                buffers,
                custom,
                &code,
                CommandInvocationKind::CallInteractively,
                &mut plan.context,
            )
            .map(|maybe_args| maybe_args.map(|args| (func, args))),
            _ => Ok(None),
        };
    }

    if let Some(bc) = func.get_bytecode_data()
        && let Some(spec) = &bc.interactive
    {
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
        if spec_val.is_nil() {
            return Ok(Some((func, Vec::new())));
        }
        if let Some(code) = spec_val.as_str() {
            if let Some(args) = interactive_args_from_string_code_in_state(
                obarray,
                dynamic,
                buffers,
                custom,
                code,
                CommandInvocationKind::CallInteractively,
                &mut plan.context,
            )? {
                return Ok(Some((func, args)));
            }
            return Ok(None);
        }
        return Ok(None);
    }

    Ok(Some((
        func,
        default_call_interactively_args_in_state(
            obarray,
            dynamic.as_slice(),
            buffers,
            frames,
            &plan.resolved_name,
        )?,
    )))
}

pub(crate) fn resolve_call_interactively_target_and_args_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    plan: &mut CallInteractivelyPlan,
    vm_gc_roots: &[Value],
) -> Result<Option<(Value, Vec<Value>)>, Flow> {
    let func = plan.func;
    if let Some(code) = shared
        .interactive
        .get_spec(&plan.resolved_name)
        .map(|spec| spec.code.clone())
    {
        if let Some(args) = interactive_args_from_string_code_in_vm_runtime(
            shared,
            &code,
            CommandInvocationKind::CallInteractively,
            &mut plan.context,
            vm_gc_roots,
        )? {
            return Ok(Some((func, args)));
        }
        return Ok(None);
    }

    if let Some(lambda) = func.get_lambda_data()
        && let Some(spec) = parsed_interactive_spec_from_lambda(lambda)
    {
        return match spec {
            ParsedInteractiveSpec::NoArgs => Ok(Some((func, Vec::new()))),
            ParsedInteractiveSpec::StringCode(code) => {
                interactive_args_from_string_code_in_vm_runtime(
                    shared,
                    &code,
                    CommandInvocationKind::CallInteractively,
                    &mut plan.context,
                    vm_gc_roots,
                )
                .map(|maybe_args| maybe_args.map(|args| (func, args)))
            }
            ParsedInteractiveSpec::Form(form) => {
                eval_interactive_form_expr_in_vm_runtime(shared, vm_gc_roots, &form)
                    .map(|args| Some((func, args)))
            }
        };
    }

    if let Some(bc) = func.get_bytecode_data()
        && let Some(spec) = &bc.interactive
    {
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
        if spec_val.is_nil() {
            return Ok(Some((func, Vec::new())));
        }
        if let Some(code) = spec_val.as_str() {
            if let Some(args) = interactive_args_from_string_code_in_vm_runtime(
                shared,
                code,
                CommandInvocationKind::CallInteractively,
                &mut plan.context,
                vm_gc_roots,
            )? {
                return Ok(Some((func, args)));
            }
            return Ok(None);
        }
        return eval_interactive_form_value_in_vm_runtime(shared, vm_gc_roots, spec_val)
            .map(|args| Some((func, args)));
    }

    Ok(None)
}

pub(crate) fn resolve_call_interactively_target_and_args_with_vm_fallback(
    shared: &mut super::eval::VmSharedState<'_>,
    plan: &mut CallInteractivelyPlan,
    vm_gc_roots: &[Value],
    extra_roots: &[Value],
) -> Result<(Value, Vec<Value>), Flow> {
    if let Some((function, call_args)) =
        resolve_call_interactively_target_and_args_in_vm_runtime(shared, plan, vm_gc_roots)?
    {
        return Ok((function, call_args));
    }

    if let Some((function, call_args)) = resolve_call_interactively_target_and_args_in_state(
        &mut *shared.obarray,
        shared.dynamic,
        shared.buffers,
        &*shared.custom,
        &*shared.frames,
        &*shared.interactive,
        plan,
    )? {
        return Ok((function, call_args));
    }

    shared.with_parent_evaluator_vm_roots(vm_gc_roots, extra_roots, move |eval| {
        resolve_call_interactively_target_and_args_in_eval(eval, plan)
    })
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
    builtin_key_binding_in_state(
        &mut eval.obarray,
        eval.dynamic.as_slice(),
        &eval.buffers,
        eval.current_local_map,
        args,
    )
}

pub(crate) fn builtin_key_binding_in_state(
    obarray: &mut Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    current_local_map: Value,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("key-binding", &args, 1)?;
    expect_max_args("key-binding", &args, 4)?;
    let string_designator = args[0].is_string();
    let no_remap = args.get(2).is_some_and(|v| v.is_truthy());
    if let Some(position) = args.get(3) {
        interactive_validate_integer_position_arg_in_buffers(buffers, position)?;
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
        let global = crate::emacs_core::builtins::keymaps::ensure_global_keymap_in_obarray(obarray);
        let mut maps = Vec::new();
        if !current_local_map.is_nil() {
            maps.push(current_local_map);
        }
        maps.push(global);
        return Ok(Value::list(maps));
    }

    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();

    let lookup_binding = |emacs_events: &[Value]| -> Option<Value> {
        if let Some(value) =
            key_binding_lookup_in_minor_mode_maps_in_state(obarray, dynamic, emacs_events)
        {
            return Some(key_binding_apply_remap_in_state(
                obarray,
                dynamic,
                current_local_map,
                value,
                no_remap,
            ));
        }

        if !current_local_map.is_nil() {
            if let Some(value) =
                key_binding_lookup_in_keymap_in_obarray(obarray, &current_local_map, emacs_events)
            {
                return Some(key_binding_apply_remap_in_state(
                    obarray,
                    dynamic,
                    current_local_map,
                    value,
                    no_remap,
                ));
            }
        }

        let global = get_global_keymap_in_obarray(obarray);
        if !global.is_nil() {
            if let Some(value) =
                key_binding_lookup_in_keymap_in_obarray(obarray, &global, emacs_events)
            {
                return Some(key_binding_apply_remap_in_state(
                    obarray,
                    dynamic,
                    current_local_map,
                    value,
                    no_remap,
                ));
            }
        }

        None
    };

    if let Some(value) = lookup_binding(&emacs_events) {
        return Ok(value);
    }

    if let Some(expanded_events) = expand_meta_prefix_char_events_in_obarray(obarray, &emacs_events)
    {
        if let Some(value) = lookup_binding(&expanded_events) {
            return Ok(value);
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
    interactive_validate_integer_position_arg_in_buffers(&eval.buffers, position)
}

fn interactive_validate_integer_position_arg_in_buffers(
    buffers: &crate::buffer::BufferManager,
    position: &Value,
) -> Result<(), Flow> {
    let Value::Int(pos) = position else {
        return Ok(());
    };
    let Some(buf) = buffers.current_buffer() else {
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
    builtin_local_key_binding_in_state(eval.current_local_map, args)
}

pub(crate) fn builtin_local_key_binding_in_state(
    current_local_map: Value,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("local-key-binding", &args, 1)?;
    expect_max_args("local-key-binding", &args, 2)?;

    if current_local_map.is_nil() {
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
        &current_local_map,
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
    key_binding_lookup_in_keymap_in_obarray(&eval.obarray, keymap, events)
}

fn key_binding_lookup_in_keymap_in_obarray(
    obarray: &Obarray,
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
            if let Some(func) = obarray.symbol_function(sym_name).copied() {
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
    get_global_keymap_in_obarray(&eval.obarray)
}

fn get_global_keymap_in_obarray(obarray: &Obarray) -> Value {
    obarray
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
    key_binding_apply_remap_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        eval.current_local_map,
        binding,
        no_remap,
    )
}

fn key_binding_apply_remap_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    current_local_map: Value,
    binding: Value,
    no_remap: bool,
) -> Value {
    if no_remap {
        return binding;
    }
    let Some(command_name) = binding.as_symbol_name().map(ToString::to_string) else {
        return binding;
    };
    match command_remapping_lookup_in_active_keymaps_in_state(
        obarray,
        dynamic,
        current_local_map,
        &command_name,
    ) {
        Some(remapped) if !remapped.is_nil() => remapped,
        _ => binding,
    }
}

fn key_binding_lookup_in_minor_mode_alist(
    eval: &Evaluator,
    events: &[Value],
    alist_value: &Value,
) -> Option<Value> {
    key_binding_lookup_in_minor_mode_alist_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        events,
        alist_value,
    )
}

fn key_binding_lookup_in_minor_mode_alist_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    events: &[Value],
    alist_value: &Value,
) -> Option<Value> {
    let entries = list_to_vec(alist_value)?;
    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_or_global_symbol_value_in_state(obarray, dynamic, &mode_name)
            .is_some_and(|v| v.is_truthy())
        {
            continue;
        }

        if !is_list_keymap(&map_value) {
            continue;
        }

        if let Some(binding) = key_binding_lookup_in_keymap_in_obarray(obarray, &map_value, events)
        {
            return Some(binding);
        }
    }
    None
}

fn key_binding_lookup_in_minor_mode_maps(eval: &Evaluator, events: &[Value]) -> Option<Value> {
    key_binding_lookup_in_minor_mode_maps_in_state(&eval.obarray, eval.dynamic.as_slice(), events)
}

fn key_binding_lookup_in_minor_mode_maps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    events: &[Value],
) -> Option<Value> {
    if let Some(emulation_raw) =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "emulation-mode-map-alists")
    {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value_in_state(obarray, dynamic, name)
                        .unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some(value) = key_binding_lookup_in_minor_mode_alist_in_state(
                    obarray,
                    dynamic,
                    events,
                    &alist_value,
                ) {
                    return Some(value);
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) =
            dynamic_or_global_symbol_value_in_state(obarray, dynamic, alist_name)
        else {
            continue;
        };
        if let Some(value) =
            key_binding_lookup_in_minor_mode_alist_in_state(obarray, dynamic, events, &alist_value)
        {
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
    lookup_minor_mode_binding_in_alist_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        events,
        alist_value,
    )
}

fn lookup_minor_mode_binding_in_alist_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
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
        if !dynamic_or_global_symbol_value_in_state(obarray, dynamic, &mode_name)
            .is_some_and(|v| v.is_truthy())
        {
            continue;
        }

        // Resolve the keymap value - could be a keymap directly or a symbol
        let keymap = if is_list_keymap(&map_value) {
            map_value
        } else if let Some(sym_name) = map_value.as_symbol_name() {
            match obarray.symbol_value(sym_name).copied() {
                Some(v) if is_list_keymap(&v) => v,
                _ => match obarray.symbol_function(sym_name).copied() {
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
    builtin_minor_mode_key_binding_in_state(&eval.obarray, eval.dynamic.as_slice(), args)
}

pub(crate) fn builtin_minor_mode_key_binding_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("minor-mode-key-binding", &args, 1)?;
    expect_max_args("minor-mode-key-binding", &args, 2)?;

    // Emacs returns nil (not a type error) for non-array key designators here.
    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(_) => return Ok(Value::Nil),
    };

    if let Some(emulation_raw) =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "emulation-mode-map-alists")
    {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value_in_state(obarray, dynamic, name)
                        .unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some((mode_name, binding)) = lookup_minor_mode_binding_in_alist_in_state(
                    obarray,
                    dynamic,
                    &events,
                    &alist_value,
                )? {
                    return Ok(Value::list(vec![Value::cons(
                        Value::symbol(mode_name),
                        binding,
                    )]));
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) =
            dynamic_or_global_symbol_value_in_state(obarray, dynamic, alist_name)
        else {
            continue;
        };
        if let Some((mode_name, binding)) =
            lookup_minor_mode_binding_in_alist_in_state(obarray, dynamic, &events, &alist_value)?
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
    builtin_this_command_keys_in_state(eval.read_command_keys(), &eval.interactive, args)
}

pub(crate) fn builtin_this_command_keys_in_state(
    read_command_keys: &[Value],
    interactive: &InteractiveRegistry,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-command-keys", &args, 0)?;
    if !read_command_keys.is_empty() {
        if let Some(rendered) = command_key_events_to_string(read_command_keys) {
            return Ok(Value::string(rendered));
        }
        return Ok(Value::vector(read_command_keys.to_vec()));
    }

    let keys = interactive.this_command_keys();
    Ok(Value::string(keys.join(" ")))
}

/// `(this-command-keys-vector)` -> vector of keys that invoked current command.
pub(crate) fn builtin_this_command_keys_vector(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_this_command_keys_vector_in_state(eval.read_command_keys(), &eval.interactive, args)
}

pub(crate) fn builtin_this_command_keys_vector_in_state(
    read_command_keys: &[Value],
    interactive: &InteractiveRegistry,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-command-keys-vector", &args, 0)?;
    if !read_command_keys.is_empty() {
        return Ok(Value::vector(read_command_keys.to_vec()));
    }

    let keys = interactive.this_command_keys();
    let vals: Vec<Value> = keys.iter().map(|k| Value::string(k.clone())).collect();
    Ok(Value::vector(vals))
}

fn single_command_key_vector_in_state(
    read_command_keys: &[Value],
    interactive: &InteractiveRegistry,
) -> Value {
    if !read_command_keys.is_empty() {
        return Value::vector(read_command_keys.to_vec());
    }

    let joined = interactive.this_command_keys().join(" ");
    if joined.is_empty() {
        return Value::vector(Vec::<Value>::new());
    }

    match super::kbd::parse_kbd_string(&joined) {
        Ok(Value::Str(id)) => {
            let text = super::value::with_heap(|h| h.get_string(id).to_owned());
            Value::vector(
                text.chars()
                    .map(|ch| Value::Int(ch as i64))
                    .collect::<Vec<_>>(),
            )
        }
        Ok(vector @ Value::Vector(_)) => vector,
        Ok(other) => Value::vector(vec![other]),
        Err(_) => Value::vector(Vec::<Value>::new()),
    }
}

fn single_command_key_vector(eval: &Evaluator) -> Value {
    single_command_key_vector_in_state(eval.read_command_keys(), &eval.interactive)
}

pub(crate) fn builtin_this_single_command_keys_in_state(
    interactive: &InteractiveRegistry,
    read_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-single-command-keys", &args, 0)?;
    Ok(single_command_key_vector_in_state(
        read_command_keys,
        interactive,
    ))
}

pub(crate) fn builtin_this_single_command_raw_keys_in_state(
    interactive: &InteractiveRegistry,
    read_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-single-command-raw-keys", &args, 0)?;
    Ok(single_command_key_vector_in_state(
        read_command_keys,
        interactive,
    ))
}

/// `(this-single-command-keys)` -> vector of keys that invoked current command.
pub(crate) fn builtin_this_single_command_keys(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_this_single_command_keys_in_state(&eval.interactive, eval.read_command_keys(), args)
}

/// `(this-single-command-raw-keys)` -> vector of raw keys for current command.
pub(crate) fn builtin_this_single_command_raw_keys(
    eval: &mut Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_this_single_command_raw_keys_in_state(&eval.interactive, eval.read_command_keys(), args)
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
    builtin_clear_this_command_keys_in_runtime(eval, args)
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

pub(crate) trait CommandKeyRuntime {
    fn read_command_keys(&self) -> &[Value];
    fn clear_command_key_state(&mut self, keep_record: bool);
}

impl CommandKeyRuntime for Evaluator {
    fn read_command_keys(&self) -> &[Value] {
        Evaluator::read_command_keys(self)
    }

    fn clear_command_key_state(&mut self, keep_record: bool) {
        Evaluator::clear_command_key_state(self, keep_record);
    }
}

impl CommandKeyRuntime for super::eval::VmSharedState<'_> {
    fn read_command_keys(&self) -> &[Value] {
        super::eval::VmSharedState::read_command_keys(self)
    }

    fn clear_command_key_state(&mut self, keep_record: bool) {
        super::eval::VmSharedState::clear_command_key_state(self, keep_record);
    }
}

pub(crate) fn builtin_clear_this_command_keys_in_runtime(
    runtime: &mut impl CommandKeyRuntime,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("clear-this-command-keys", &args, 1)?;
    let keep_record = args.first().is_some_and(|arg| arg.is_truthy());
    runtime.clear_command_key_state(keep_record);
    Ok(Value::Nil)
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

fn command_remapping_keymap_arg_valid(value: &Value) -> bool {
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
    command_remapping_lookup_in_minor_mode_alist_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        command_name,
        alist_value,
    )
}

fn command_remapping_lookup_in_minor_mode_alist_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    command_name: &str,
    alist_value: &Value,
) -> Option<Value> {
    let entries = list_to_vec(alist_value)?;
    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_or_global_symbol_value_in_state(obarray, dynamic, &mode_name)
            .is_some_and(|v| v.is_truthy())
        {
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
    command_remapping_lookup_in_minor_mode_maps_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        command_name,
    )
}

fn command_remapping_lookup_in_minor_mode_maps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    command_name: &str,
) -> Option<Value> {
    if let Some(emulation_raw) =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "emulation-mode-map-alists")
    {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for emulation_entry in emulation_entries {
                let alist_value = match emulation_entry.as_symbol_name() {
                    Some(name) => dynamic_or_global_symbol_value_in_state(obarray, dynamic, name)
                        .unwrap_or(Value::Nil),
                    None => emulation_entry,
                };
                if let Some(value) = command_remapping_lookup_in_minor_mode_alist_in_state(
                    obarray,
                    dynamic,
                    command_name,
                    &alist_value,
                ) {
                    return Some(value);
                }
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) =
            dynamic_or_global_symbol_value_in_state(obarray, dynamic, alist_name)
        else {
            continue;
        };
        if let Some(value) = command_remapping_lookup_in_minor_mode_alist_in_state(
            obarray,
            dynamic,
            command_name,
            &alist_value,
        ) {
            return Some(value);
        }
    }

    None
}

fn command_remapping_lookup_in_active_keymaps(
    eval: &Evaluator,
    command_name: &str,
) -> Option<Value> {
    command_remapping_lookup_in_active_keymaps_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        eval.current_local_map,
        command_name,
    )
}

fn command_remapping_lookup_in_active_keymaps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    current_local_map: Value,
    command_name: &str,
) -> Option<Value> {
    if let Some(value) =
        command_remapping_lookup_in_minor_mode_maps_in_state(obarray, dynamic, command_name)
    {
        return Some(value);
    }
    if is_list_keymap(&current_local_map) {
        if let Some(value) =
            command_remapping_lookup_in_keymap_value(&current_local_map, command_name)
        {
            return Some(value);
        }
    }
    let global_map = get_global_keymap_in_obarray(obarray);
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
