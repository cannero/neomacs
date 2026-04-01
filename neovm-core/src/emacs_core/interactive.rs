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
use super::eval::{Context, quote_to_value, value_to_expr};
use super::expr::Expr;
use super::intern::{intern, resolve_sym};
use super::keyboard::pure::make_event_array_value;
use super::keymap::{
    KeyEvent, command_remapping_command_name as keymap_command_remapping_command_name,
    command_remapping_lookup_in_keymaps as keymap_command_remapping_lookup_in_keymaps,
    command_remapping_lookup_in_lisp_keymap as keymap_command_remapping_lookup_in_lisp_keymap,
    command_remapping_normalize_target as keymap_command_remapping_normalize_target,
    current_active_maps_for_position, current_active_maps_for_position_read_only,
    expand_meta_prefix_char_events_in_obarray, format_key_event, format_key_sequence,
    is_list_keymap, key_event_to_emacs_event, list_keymap_for_each_binding,
    lookup_keymap_with_partial, make_sparse_list_keymap, minor_mode_key_binding_in_context,
    resolve_active_key_binding, where_is_keymaps_in_context,
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
        Value::NIL
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
        "ignore" => Some(interactive_form_from_spec_value(Value::NIL)),
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
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
pub(crate) fn builtin_call_interactively(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("call-interactively", &args, 1)?;
    expect_max_args("call-interactively", &args, 3)?;
    expect_optional_command_keys_vector(args.get(2))?;

    let func_val = args[0];

    // GNU Emacs (callint.c:310-315, eval.c:2268-2376):
    // `call-interactively` checks `commandp` which itself may call
    // `(interactive-form fun)` for genfun dispatch (oclosures, advice
    // wrappers, etc.).  The _in_state commandp can't call Elisp, so we
    // do the full check here: first try the fast _in_state path, then
    // fall back to calling `(interactive-form fun)` which handles ALL
    // cases — oclosures, advice, nadvice wrappers, etc.
    let is_command = {
        let fast =
            command_designator_p_in_state(&eval.obarray, &eval.interactive, &func_val, false);
        if fast {
            true
        } else {
            // Evaluator-backed fallback: call `(interactive-form fun)`.
            // This mirrors GNU's genfun path and handles any function type
            // that declares interactivity via cl-generic methods.
            eval.apply(Value::symbol("interactive-form"), vec![func_val])
                .is_ok_and(|v| !v.is_nil())
        }
    };

    if !is_command {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("commandp"), func_val],
        ));
    }

    let Some((resolved_name, func)) = resolve_command_target(&eval, &func_val) else {
        return Err(signal("void-function", vec![func_val]));
    };
    let context =
        InteractiveInvocationContext::from_keys_arg_in_state(eval.read_command_keys(), args.get(2));
    finish_call_interactively_in_eval(
        eval,
        CallInteractivelyPlan {
            resolved_name,
            func,
            context,
        },
    )
}

/// `(interactive-p)` -> t if the calling function was called interactively.
pub(crate) fn builtin_interactive_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_args("interactive-p", &args, 0)?;
    let _ = eval;
    // Emacs 30 keeps `interactive-p` obsolete; it effectively returns nil.
    Ok(Value::NIL)
}

/// `(called-interactively-p &optional KIND)`
/// Return t if the calling function was called interactively.
/// KIND can be 'interactive or 'any.
pub(crate) fn builtin_called_interactively_p(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    // Accept 0 or 1 args
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("called-interactively-p"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    if !eval.interactive.is_called_interactively() {
        return Ok(Value::NIL);
    }

    // GNU Emacs semantics:
    // - KIND = 'interactive => nil
    // - KIND = nil / 'any / unknown => t (when called interactively)
    if args
        .first()
        .is_some_and(|v| v.is_symbol_named("interactive"))
    {
        Ok(Value::NIL)
    } else {
        Ok(Value::T)
    }
}

/// `(commandp FUNCTION &optional FOR-CALL-INTERACTIVELY)`
/// Return non-nil if FUNCTION is a command (i.e., can be called interactively).
///
/// Matches GNU Emacs eval.c:2268-2376.  Uses the fast _in_state check first,
/// then falls back to `(interactive-form fun)` for genfun/oclosure dispatch.
pub(crate) fn builtin_commandp_interactive(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("commandp", &args, 1)?;
    expect_max_args("commandp", &args, 2)?;
    let for_call_interactively = args.get(1).is_some_and(|value| !value.is_nil());

    // Fast path: _in_state check (handles most cases without Elisp calls)
    let fast = command_designator_p_in_state(
        &eval.obarray,
        &eval.interactive,
        &args[0],
        for_call_interactively,
    );
    if fast {
        return Ok(Value::T);
    }

    // Slow path: call `(interactive-form fun)` which handles genfun
    // dispatch (oclosures, advice wrappers, etc.)
    // GNU Emacs (eval.c:2348-2364): genfun path calls interactive-form.
    if let Ok(iform) = eval.apply(Value::symbol("interactive-form"), vec![args[0]]) {
        if !iform.is_nil() {
            return Ok(if for_call_interactively {
                Value::NIL
            } else {
                Value::T
            });
        }
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_commandp_impl(
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
    Ok(Value::bool_val(is_command))
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
            Value::NIL
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
    if !form.is_cons() {
        return Ok(None);
    };
    let pair_car = form.cons_car();
    let pair_cdr = form.cons_cdr();
    if pair_car.as_symbol_name() != Some("interactive") {
        return Ok(None);
    }

    match pair_cdr.kind() {
        ValueKind::Nil => Ok(Some(Value::NIL)),
        ValueKind::Cons => {
            let arg_pair_car = pair_cdr.cons_car();
            let arg_pair_cdr = pair_cdr.cons_cdr();
            Ok(Some(arg_pair_cdr))
        }
        tail => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), pair_cdr],
        )),
    }
}

fn command_modes_from_quoted_lambda(value: &Value) -> Result<Option<Value>, Flow> {
    let Some(items) = value_list_to_vec(value) else {
        return Ok(None);
    };
    if items.first().and_then(|v| v.as_symbol_name()) != Some("lambda") {
        return Ok(None);
    }

    let mut body_index = 2;
    if items.get(body_index).is_some_and(|v| v.is_string()) {
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

pub(crate) fn builtin_command_modes_impl(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("command-modes", args, 1)?;
    let command = args[0];
    let mut function = command;

    if let Some(mut current) = crate::emacs_core::builtins::symbols::symbol_id(&command) {
        let Some((_, indirect_function)) =
            crate::emacs_core::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
                obarray, current,
            )
        else {
            return Ok(Value::NIL);
        };
        if indirect_function.is_nil() {
            return Ok(Value::NIL);
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
                return Ok(Value::NIL);
            };
            function = next_function;
            let Some(next_symbol) = crate::emacs_core::builtins::symbols::symbol_id(&function)
            else {
                break;
            };
            current = next_symbol;
        }
    }

    match function.kind() {
        ValueKind::Subr(_) => Ok(Value::NIL),
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
            let Some(lambda) = function.get_lambda_data() else {
                return Ok(Value::NIL);
            };
            let body = lambda.body.clone();
            Ok(command_modes_from_expr_body(&body).unwrap_or(Value::NIL))
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            let Some(bc) = function.get_bytecode_data() else {
                return Ok(Value::NIL);
            };
            let interactive = bc.interactive;
            let Some(ref int_val) = interactive else {
                return Ok(Value::NIL);
            };
            if !int_val.is_vector() {
                return Ok(Value::NIL);
            }
            let vec_data = int_val.as_vector_data().unwrap();
            Ok(if vec_data.len() > 1 {
                unquote_command_modes_value(vec_data[1])
            } else {
                Value::NIL
            })
        }
        ValueKind::Cons if super::autoload::is_autoload_value(&function) => {
            let Some(items) = value_list_to_vec(&function) else {
                return Ok(Value::NIL);
            };
            Ok(match items.get(3).copied() {
                Some(v) if v.is_cons() => v,
                _ => Value::NIL,
            })
        }
        ValueKind::Cons => Ok(command_modes_from_quoted_lambda(&function)?.unwrap_or(Value::NIL)),
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_command_modes(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_command_modes_impl(&eval.obarray, &args)
}

/// `(command-remapping COMMAND &optional POSITION KEYMAP)` -- return remapped
/// command for COMMAND.
///
/// Respects local/global keymaps when KEYMAP is omitted or nil.
pub(crate) fn builtin_command_remapping(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_command_remapping_impl(eval, args)
}

pub(crate) fn builtin_command_remapping_impl(
    ctx: &crate::emacs_core::eval::Context,
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
        return Ok(Value::NIL);
    };
    if let Some(keymap_arg) = args.get(2) {
        match keymap_arg.kind() {
            ValueKind::Cons => {
                if let Some(target) =
                    command_remapping_lookup_in_lisp_keymap(keymap_arg, &command_name)
                {
                    return Ok(command_remapping_normalize_target(target));
                }
                return Ok(Value::NIL);
            }
            ValueKind::Nil => {
                let active_maps =
                    current_active_maps_for_position_read_only(ctx, true, args.get(1))?;
                return Ok(
                    command_remapping_lookup_in_keymaps(&active_maps, &command_name)
                        .unwrap_or(Value::NIL),
                );
            }
            _ => {
                // Not a valid keymap
                return Ok(Value::NIL);
            }
        }
    }
    let active_maps = current_active_maps_for_position_read_only(ctx, true, args.get(1))?;
    Ok(command_remapping_lookup_in_keymaps(&active_maps, &command_name).unwrap_or(Value::NIL))
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
        match cursor.kind() {
            ValueKind::Nil => return Some(values),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                values.push(pair_car);
                cursor = pair_cdr;
            }
            _ => return None,
        }
    }
}

fn value_is_interactive_form(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            pair_car.as_symbol_name() == Some("interactive")
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
    items.get(3).is_some_and(|v| !v.is_nil())
}

fn quoted_lambda_has_interactive_form(value: &Value) -> bool {
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    if items.first().and_then(|v| v.as_symbol_name()) != Some("lambda") {
        return false;
    }

    let mut body_index = 2;
    if items.get(body_index).is_some_and(|v| v.is_string()) {
        body_index += 1;
    }
    while items.get(body_index).is_some_and(value_is_declare_form) {
        body_index += 1;
    }

    items.get(body_index).is_some_and(value_is_interactive_form)
}

fn value_is_declare_form(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            pair_car.as_symbol_name() == Some("declare")
        }
        _ => false,
    }
}

fn resolve_function_designator_symbol(eval: &Context, name: &str) -> Option<(String, Value)> {
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

    match value.kind() {
        ValueKind::Veclike(VecLikeType::Lambda) => {
            if let Some(lambda) = value.get_lambda_data() {
                // GNU Emacs checks closure vector size (PVSIZE > CLOSURE_INTERACTIVE)
                // to detect interactive closures.  We check the dedicated field first,
                // then fall back to body scanning for closures created without
                // the field (e.g., dynamically-scoped lambdas, pdump closures).
                if lambda.interactive.is_some() || lambda_body_has_interactive_form(&lambda.body) {
                    return true;
                }
                // GNU Emacs (eval.c:2304-2314): For closures where doc_form
                // is not a valid docstring (i.e., an oclosure type symbol),
                // GNU sets genfun=true and calls `(interactive-form fun)`.
                // This handles advice wrappers and other oclosures.
                // We can't call Elisp from _in_state, so approximate:
                // if this is an oclosure (doc_form is a symbol), treat it
                // as potentially interactive if the resolved symbol name
                // is registered as interactive.
                if lambda
                    .doc_form
                    .is_some_and(|v| v.as_symbol_name().is_some())
                {
                    if let Some(name) = resolved_name {
                        if interactive.is_interactive(name) {
                            return true;
                        }
                    }
                }
                false
            } else {
                false
            }
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => value
            .get_bytecode_data()
            .is_some_and(|bc| bc.interactive.is_some()),
        ValueKind::Cons => quoted_lambda_has_interactive_form(value),
        ValueKind::Subr(id) => {
            let name = resolve_sym(id);
            interactive.is_interactive(name) || builtin_command_name(name)
        }
        ValueKind::String | ValueKind::Veclike(VecLikeType::Vector) => !for_call_interactively,
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

fn command_object_p(eval: &Context, resolved_name: Option<&str>, value: &Value) -> bool {
    command_object_p_in_state(&eval.interactive, resolved_name, value, false)
}

fn command_designator_p(eval: &Context, designator: &Value) -> bool {
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
    fn from_keys_arg(eval: &Context, keys: Option<&Value>) -> Self {
        Self::from_keys_arg_in_state(eval.read_command_keys(), keys)
    }

    fn from_keys_arg_in_state(read_command_keys: &[Value], keys: Option<&Value>) -> Self {
        let mut context = Self::default();
        if let Some(keys_val) = keys {
            if keys_val.is_vector() {
                if let Some(vec_data) = keys_val.as_vector_data() {
                    if !vec_data.is_empty() {
                        context.command_keys = vec_data.clone();
                        context.has_command_keys_context = true;
                        return context;
                    }
                }
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
    match event.kind() {
        ValueKind::Symbol(id) => Some(resolve_sym(id)),
        ValueKind::Cons => {
            let car = event.cons_car();
            match car.kind() {
                ValueKind::Symbol(id) => Some(resolve_sym(id)),
                _ => None,
            }
        }
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
    match sequence.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            sequence.as_vector_data().and_then(|v| v.last().copied())
        }
        _ => None,
    }
}

fn interactive_capture_up_event_in_eval(
    eval: &mut Context,
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
    shared: &mut super::eval::Context,
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
        .unwrap_or(Value::NIL)
}

fn dynamic_or_global_symbol_value(eval: &Context, name: &str) -> Option<Value> {
    eval.obarray.symbol_value(name).cloned()
}

fn dynamic_or_global_symbol_value_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    obarray.symbol_value(name).cloned()
}

fn dynamic_buffer_or_global_symbol_value(
    eval: &Context,
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    dynamic_buffer_or_global_symbol_value_in_state(&eval.obarray, &[], buf, name)
}

fn dynamic_buffer_or_global_symbol_value_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buf: &crate::buffer::Buffer,
    name: &str,
) -> Option<Value> {
    if let Some(v) = buf.get_buffer_local(name) {
        return Some(*v);
    }
    obarray.symbol_value(name).cloned()
}

fn prefix_numeric_value(value: &Value) -> i64 {
    match value.kind() {
        ValueKind::Nil => 1,
        ValueKind::Fixnum(n) => n,
        ValueKind::Float => value.xfloat() as i64,
        ValueKind::Char(c) => c as i64,
        ValueKind::Symbol(id) if resolve_sym(id) == "-" => -1,
        ValueKind::Cons => {
            let car = {
                let pair_car = value.cons_car();
                let pair_cdr = value.cons_cdr();
                pair_car
            };
            match car.kind() {
                ValueKind::Fixnum(n) => n,
                ValueKind::Float => car.xfloat() as i64,
                ValueKind::Char(c) => c as i64,
                _ => 1,
            }
        }
        _ => 1,
    }
}

fn interactive_prefix_raw_arg(eval: &Context, kind: CommandInvocationKind) -> Value {
    interactive_prefix_raw_arg_in_state(&eval.obarray, &[], kind)
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
    dynamic_or_global_symbol_value_in_state(obarray, dynamic, symbol).unwrap_or(Value::NIL)
}

fn interactive_prefix_numeric_arg(eval: &Context, kind: CommandInvocationKind) -> Value {
    let raw = interactive_prefix_raw_arg(eval, kind);
    Value::fixnum(prefix_numeric_value(&raw))
}

fn interactive_prefix_numeric_arg_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    kind: CommandInvocationKind,
) -> Value {
    let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic, kind);
    Value::fixnum(prefix_numeric_value(&raw))
}

fn interactive_region_args(eval: &Context, missing_mark_signal: &str) -> Result<Vec<Value>, Flow> {
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
    Ok(vec![Value::fixnum(beg_char), Value::fixnum(end_char)])
}

fn interactive_point_arg(eval: &Context) -> Result<Value, Flow> {
    interactive_point_arg_in_buffers(&eval.buffers)
}

fn interactive_point_arg_in_buffers(buffers: &crate::buffer::BufferManager) -> Result<Value, Flow> {
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_char = buf.point_char() as i64 + 1;
    Ok(Value::fixnum(point_char))
}

fn interactive_mark_arg(eval: &Context) -> Result<Value, Flow> {
    interactive_mark_arg_in_buffers(&eval.buffers)
}

fn interactive_mark_arg_in_buffers(buffers: &crate::buffer::BufferManager) -> Result<Value, Flow> {
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.mark()
        .ok_or_else(|| signal("error", vec![Value::string("The mark is not set now")]))?;
    let mark_char = buf.mark_char().expect("mark byte/char stay in sync") as i64 + 1;
    Ok(Value::fixnum(mark_char))
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
    specpdl: &[crate::emacs_core::eval::SpecBinding],
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
        specpdl,
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
            'i' => args.push(Value::NIL),
            'm' => args.push(interactive_mark_arg_in_buffers(buffers)?),
            'N' => {
                let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic.as_slice(), kind);
                if raw.is_nil() {
                    return Ok(None);
                }
                args.push(Value::fixnum(prefix_numeric_value(&raw)));
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
            'U' => args.push(Value::NIL),
            'Z' => {
                let raw = interactive_prefix_raw_arg_in_state(obarray, dynamic.as_slice(), kind);
                if raw.is_nil() {
                    args.push(Value::NIL);
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
    shared: &mut super::eval::Context,
    code: &str,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
    vm_gc_roots: &[Value],
) -> Result<Option<Vec<Value>>, Flow> {
    let parsed = parse_interactive_code_entries(code);
    interactive_apply_prefix_flags(shared, &parsed.prefix_flags, context)?;
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
                let letter_args = [Value::string(prompt), Value::NIL, Value::T];
                super::minibuffer::builtin_read_buffer_in_runtime(shared, &letter_args)?;
                let completing_args = {
                    super::minibuffer::read_buffer_completing_args(&shared.buffers, &letter_args)
                };
                args.push(super::reader::finish_completing_read_in_vm_runtime(
                    shared,
                    vm_gc_roots,
                    &completing_args,
                )?);
            }
            'B' => {
                let letter_args = [Value::string(prompt), Value::NIL, Value::NIL];
                super::minibuffer::builtin_read_buffer_in_runtime(shared, &letter_args)?;
                let completing_args = {
                    super::minibuffer::read_buffer_completing_args(&shared.buffers, &letter_args)
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
            'd' => args.push(interactive_point_arg_in_buffers(&shared.buffers)?),
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
                    &mut shared.obarray,
                    &[],
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
                let letter_args = [Value::string(prompt), Value::NIL, Value::NIL, Value::T];
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
            'i' => args.push(Value::NIL),
            'k' => {
                let letter_args = [Value::string(prompt)];
                let arg = if let Some(arg) =
                    super::reader::builtin_read_key_sequence_in_runtime(shared, &letter_args)?
                {
                    arg
                } else {
                    super::reader::finish_read_key_sequence_interactive_in_runtime(
                        shared,
                        super::reader::read_key_sequence_options_from_args(&letter_args),
                    )?
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
                    super::reader::finish_read_key_sequence_vector_interactive_in_runtime(
                        shared,
                        super::reader::read_key_sequence_options_from_args(&letter_args),
                    )?
                };
                interactive_capture_up_event_in_vm_batch_runtime(shared, &arg, context)?;
                args.push(arg);
            }
            'M' => {
                let letter_args = [
                    Value::string(prompt),
                    Value::NIL,
                    Value::NIL,
                    Value::NIL,
                    Value::T,
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
            'm' => args.push(interactive_mark_arg_in_buffers(&shared.buffers)?),
            'N' => {
                let raw = interactive_prefix_raw_arg_in_state(&shared.obarray, &[], kind);
                if raw.is_nil() {
                    let letter_args = [Value::string(prompt)];
                    args.push(super::reader::finish_read_number_in_vm_runtime(
                        shared,
                        vm_gc_roots,
                        &letter_args,
                    )?);
                } else {
                    args.push(Value::fixnum(prefix_numeric_value(&raw)));
                }
            }
            'p' => args.push(interactive_prefix_numeric_arg_in_state(
                &shared.obarray,
                &[],
                kind,
            )),
            'P' => args.push(interactive_prefix_raw_arg_in_state(
                &shared.obarray,
                &[],
                kind,
            )),
            'r' => args.extend(interactive_region_args_in_buffers(
                &shared.buffers,
                "error",
            )?),
            'R' => {
                if interactive_use_region_p_in_vm_runtime(shared, vm_gc_roots)? {
                    args.extend(interactive_region_args_in_buffers(
                        &shared.buffers,
                        "error",
                    )?);
                } else {
                    args.push(Value::NIL);
                    args.push(Value::NIL);
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
            'x' => args.push(interactive_read_expression_arg_in_vm_runtime(
                shared,
                vm_gc_roots,
                prompt,
            )?),
            'X' => args.push(interactive_eval_expression_arg_in_vm_runtime(
                shared,
                vm_gc_roots,
                prompt,
            )?),
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
                let raw = interactive_prefix_raw_arg_in_state(&shared.obarray, &[], kind);
                if raw.is_nil() {
                    args.push(Value::NIL);
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
        | "scroll-down-command" => Ok(vec![Value::fixnum(1)]),
        "kill-region" => interactive_region_args_in_buffers(buffers, "user-error"),
        "kill-ring-save" => interactive_region_args_in_buffers(buffers, "error"),
        "copy-region-as-kill" => interactive_region_args_in_buffers(buffers, "error"),
        "set-mark-command" => Ok(vec![Value::NIL]),
        "split-window-below" | "split-window-right" => {
            let win = frames
                .selected_frame()
                .map(|f| Value::make_window(f.selected_window.0))
                .unwrap_or(Value::NIL);
            Ok(vec![Value::NIL, win])
        }
        "capitalize-region" => interactive_region_args_in_buffers(buffers, "error"),
        "upcase-initials-region" => interactive_region_args_in_buffers(buffers, "error"),
        "upcase-region" | "downcase-region" => Err(signal(
            "args-out-of-range",
            vec![Value::string(""), Value::fixnum(0)],
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

fn interactive_read_expression_arg(eval: &mut Context, prompt: String) -> Result<Value, Flow> {
    let input = super::reader::builtin_read_from_minibuffer(eval, vec![Value::string(prompt)])?;
    super::reader::builtin_read(eval, vec![input])
}

fn interactive_read_expression_arg_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    prompt: String,
) -> Result<Value, Flow> {
    let input = super::reader::finish_read_from_minibuffer_in_vm_runtime(
        shared,
        vm_gc_roots,
        &[Value::string(prompt)],
    )?;
    super::reader::builtin_read(shared, vec![input])
}

fn interactive_eval_expression_arg_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    prompt: String,
) -> Result<Value, Flow> {
    shared.with_extra_gc_roots(vm_gc_roots, &[], move |eval| {
        let expr_value = interactive_read_expression_arg(eval, prompt)?;
        eval.eval_value(&expr_value)
    })
}

fn interactive_read_coding_system_optional_arg(prompt: String) -> Result<Value, Flow> {
    match super::lread::builtin_read_coding_system(vec![Value::string(prompt)]) {
        Ok(value) => Ok(value),
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file" => Ok(Value::NIL),
        Err(flow) => Err(flow),
    }
}

fn interactive_use_region_p_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
) -> Result<bool, Flow> {
    shared
        .with_extra_gc_roots(vm_gc_roots, &[], |eval| {
            eval.apply(Value::symbol("use-region-p"), vec![])
        })
        .map(|value| value.is_truthy())
}

fn interactive_buffer_read_only_active(eval: &Context, buf: &crate::buffer::Buffer) -> bool {
    interactive_buffer_read_only_active_in_state(&eval.obarray, &[], buf)
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

fn interactive_require_writable_current_buffer(eval: &Context) -> Result<(), Flow> {
    interactive_require_writable_current_buffer_in_state(&eval.obarray, &[], &eval.buffers)
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

fn interactive_apply_shift_selection_prefix(eval: &mut Context) {
    interactive_apply_shift_selection_prefix_in_state(
        &mut eval.obarray,
        &mut [],
        &mut eval.buffers,
        &eval.custom,
        eval.specpdl.as_slice(),
    );
}

fn interactive_apply_shift_selection_prefix_in_state(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    specpdl: &[crate::emacs_core::eval::SpecBinding],
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
        let _ = buffers.set_buffer_local_property(current_id, "mark-active", Value::T);
        mark_activated = true;
    }
    if mark_activated {
        let _ = super::eval::set_runtime_binding(
            obarray,
            buffers,
            custom,
            specpdl,
            intern("mark-active"),
            Value::T,
        );
    }
}

fn interactive_first_event_with_parameters_from_keys(
    context: &InteractiveInvocationContext,
) -> Option<Value> {
    context
        .command_keys
        .iter()
        .copied()
        .find(interactive_event_with_parameters_p)
}

fn interactive_first_event_with_parameters(
    eval: &Context,
    context: &InteractiveInvocationContext,
) -> Option<Value> {
    if context.has_command_keys_context {
        return interactive_first_event_with_parameters_from_keys(context);
    }
    interactive_last_input_event_with_parameters(eval)
}

fn interactive_event_target_window(event: &Value) -> Option<Value> {
    let event_slots = crate::emacs_core::value::list_to_vec(event)?;
    let mut position = *event_slots.get(1)?;
    if let Some(positions) = crate::emacs_core::value::list_to_vec(&position)
        && let Some(first_position) = positions.first()
    {
        position = *first_position;
    }
    let position_slots = crate::emacs_core::value::list_to_vec(&position)?;
    let first = *position_slots.first()?;
    if first.is_window() { Some(first) } else { None }
}

fn interactive_inactive_minibuffer_target_p(
    eval: &Context,
    window_id: crate::window::WindowId,
) -> bool {
    eval.frames.frame_list().into_iter().any(|frame_id| {
        eval.frames
            .get(frame_id)
            .is_some_and(|frame| frame.minibuffer_window == Some(window_id))
    }) && eval.active_minibuffer_window != Some(window_id)
}

fn interactive_select_window_from_prefix_context(
    eval: &mut Context,
    context: &InteractiveInvocationContext,
) -> Result<(), Flow> {
    let Some(event) = interactive_first_event_with_parameters(eval, context) else {
        return Ok(());
    };
    let Some(window_value) = interactive_event_target_window(&event) else {
        return Ok(());
    };
    if !window_value.is_window() {
        return Ok(());
    };
    let Some(wid) = window_value.as_window_id() else {
        return Ok(());
    };
    let window_id = crate::window::WindowId(wid);
    if interactive_inactive_minibuffer_target_p(eval, window_id) {
        return Err(signal(
            "error",
            vec![Value::string(
                "Attempt to select inactive minibuffer window",
            )],
        ));
    }

    eval.run_hook_if_bound("mouse-leave-buffer-hook")?;
    crate::emacs_core::window_cmds::builtin_select_window(eval, vec![window_value, Value::NIL])?;
    Ok(())
}

fn interactive_apply_prefix_flags(
    eval: &mut Context,
    prefix_flags: &[char],
    context: &InteractiveInvocationContext,
) -> Result<(), Flow> {
    for prefix_flag in prefix_flags {
        match prefix_flag {
            '*' => interactive_require_writable_current_buffer_in_state(
                &mut eval.obarray,
                &mut [],
                &mut eval.buffers,
            )?,
            '@' => interactive_select_window_from_prefix_context(eval, context)?,
            '^' => interactive_apply_shift_selection_prefix_in_state(
                &mut eval.obarray,
                &mut [],
                &mut eval.buffers,
                &eval.custom,
                eval.specpdl.as_slice(),
            ),
            _ => {}
        }
    }
    Ok(())
}

fn interactive_apply_prefix_flags_in_state(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    specpdl: &[crate::emacs_core::eval::SpecBinding],
    prefix_flags: &[char],
) -> Result<(), Flow> {
    for prefix_flag in prefix_flags {
        match prefix_flag {
            '*' => interactive_require_writable_current_buffer_in_state(obarray, dynamic, buffers)?,
            '@' => {
                // Selecting the window from the first mouse event requires command-loop
                // event context; current batch paths have no such events yet.
            }
            '^' => interactive_apply_shift_selection_prefix_in_state(
                obarray, dynamic, buffers, custom, specpdl,
            ),
            _ => {}
        }
    }
    Ok(())
}

fn interactive_event_with_parameters_p(event: &Value) -> bool {
    event.is_cons()
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

fn interactive_last_input_event_with_parameters(eval: &Context) -> Option<Value> {
    let event = dynamic_or_global_symbol_value(eval, "last-input-event")?;
    interactive_event_with_parameters_p(&event).then_some(event)
}

fn interactive_next_event_with_parameters(
    eval: &Context,
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

/// Parse interactive spec from a Value (from LambdaData.interactive or bytecode).
/// The value is the SPEC part (already extracted from `(interactive SPEC)`).
fn parse_interactive_spec_from_value(spec: &Value) -> Option<ParsedInteractiveSpec> {
    match spec.kind() {
        ValueKind::Nil => Some(ParsedInteractiveSpec::NoArgs),
        ValueKind::String => {
            let s = spec.as_str().unwrap().to_owned();
            if s.is_empty() {
                Some(ParsedInteractiveSpec::NoArgs)
            } else {
                Some(ParsedInteractiveSpec::StringCode(s))
            }
        }
        _ => {
            // Could be a form to evaluate
            let expr = super::eval::value_to_expr(spec);
            Some(ParsedInteractiveSpec::Form(expr))
        }
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
    eval: &mut Context,
    code: &str,
    kind: CommandInvocationKind,
    context: &mut InteractiveInvocationContext,
) -> Result<Option<Vec<Value>>, Flow> {
    let parsed = parse_interactive_code_entries(code);
    interactive_apply_prefix_flags(eval, &parsed.prefix_flags, context)?;
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
                vec![Value::string(prompt), Value::NIL, Value::T],
            )?),
            'B' => args.push(super::minibuffer::builtin_read_buffer(
                eval,
                vec![Value::string(prompt), Value::NIL, Value::NIL],
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
                vec![Value::string(prompt), Value::NIL, Value::NIL, Value::T],
            )?),
            'F' => args.push(super::minibuffer::builtin_read_file_name(
                eval,
                vec![Value::string(prompt)],
            )?),
            'G' => args.push(super::minibuffer::builtin_read_file_name(
                eval,
                vec![Value::string(prompt)],
            )?),
            'i' => args.push(Value::NIL),
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
                    args.push(Value::fixnum(prefix_numeric_value(&raw)));
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
                    args.push(Value::NIL);
                    args.push(Value::NIL);
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
                    args.push(Value::NIL);
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
    eval: &mut Context,
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
        // Check LambdaData.interactive field first (mirrors GNU closure slot 5).
        // This handles the case where cconv stripped (interactive ...) from the
        // body but preserved it in the iform parameter.
        if let Some(ref iform_val) = lambda.interactive {
            let spec = parse_interactive_spec_from_value(iform_val);
            if let Some(spec) = spec {
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
            } else {
                // interactive field exists but spec is nil/empty → no args
                return Ok(Vec::new());
            }
        }
        // Fall back to scanning the body
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
            let spec_val = if spec.is_vector() {
                if let Some(vec_data) = spec.as_vector_data() {
                    if !vec_data.is_empty() {
                        vec_data[0]
                    } else {
                        *spec
                    }
                } else {
                    *spec
                }
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

    // GNU Emacs genfun fallback: call `(interactive-form func)` which handles
    // oclosures, advice wrappers, and other cl-generic dispatched interactivity.
    // This mirrors GNU callint.c which calls Finteractive_form to get the spec.
    if let Ok(iform) = eval.apply(Value::symbol("interactive-form"), vec![*func]) {
        if iform.is_cons() {
            // iform = (interactive SPEC)
            let spec_val = iform.cons_cdr();
            if spec_val.is_cons() {
                let spec = spec_val.cons_car();
                if let Some(s) = spec.as_str() {
                    if let Some(args) = interactive_args_from_string_code(eval, s, kind, context)? {
                        return Ok(args);
                    }
                } else if spec.is_nil() {
                    return Ok(Vec::new());
                } else {
                    let value = eval.eval_value(&spec)?;
                    return Ok(interactive_form_value_to_args(value)?);
                }
            } else {
                // (interactive) with no spec
                return Ok(Vec::new());
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
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    form: &Expr,
) -> Result<Vec<Value>, Flow> {
    let value = shared.with_extra_gc_roots(vm_gc_roots, &[], move |eval| eval.eval(form))?;
    interactive_form_value_to_args(value)
}

fn eval_interactive_form_value_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    form: Value,
) -> Result<Vec<Value>, Flow> {
    let value =
        shared.with_extra_gc_roots(vm_gc_roots, &[form], move |eval| eval.eval_value(&form))?;
    interactive_form_value_to_args(value)
}

pub(crate) fn callable_form_needs_instantiation(value: &Value) -> bool {
    let Some(items) = value_list_to_vec(value) else {
        return false;
    };
    matches!(
        items.first().and_then(|v| v.as_symbol_name()),
        Some("lambda" | "closure")
    )
}

fn normalize_command_callable(eval: &mut Context, value: Value) -> Result<Value, Flow> {
    if callable_form_needs_instantiation(&value) {
        return eval.eval_value(&value);
    }
    Ok(value)
}

fn default_command_execute_args(eval: &Context, name: &str) -> Result<Vec<Value>, Flow> {
    default_command_execute_args_in_state(&eval.buffers, &eval.frames, name)
}

fn default_call_interactively_args(eval: &Context, name: &str) -> Result<Vec<Value>, Flow> {
    default_call_interactively_args_in_state(&eval.obarray, &[], &eval.buffers, &eval.frames, name)
}

fn resolve_command_target(eval: &Context, designator: &Value) -> Option<(String, Value)> {
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
            return Some((name.to_string(), Value::subr(intern(name))));
        }
        return None;
    }
    match designator.kind() {
        ValueKind::Subr(id) => Some((resolve_sym(id).to_owned(), *designator)),
        ValueKind::T => Some(("t".to_string(), *designator)),
        ValueKind::Keyword(id) => Some((resolve_sym(id).to_owned(), *designator)),
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
    eval: &mut Context,
    mut plan: CallInteractivelyPlan,
) -> EvalResult {
    let (func, call_args) = resolve_call_interactively_target_and_args_in_eval(eval, &mut plan)?;
    let mut funcall_args = Vec::with_capacity(call_args.len() + 1);
    funcall_args.push(func);
    funcall_args.extend(call_args);
    eval.apply(Value::symbol("funcall-interactively"), funcall_args)
}

pub(crate) fn resolve_call_interactively_target_and_args_in_eval(
    eval: &mut Context,
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
    specpdl: &[crate::emacs_core::eval::SpecBinding],
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
            specpdl,
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
                specpdl,
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
        let spec_val = if spec.is_vector() {
            if let Some(vec_data) = spec.as_vector_data() {
                if !vec_data.is_empty() {
                    vec_data[0]
                } else {
                    *spec
                }
            } else {
                *spec
            }
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
                specpdl,
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
    shared: &mut super::eval::Context,
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
        let spec_val = if spec.is_vector() {
            if let Some(vec_data) = spec.as_vector_data() {
                if !vec_data.is_empty() {
                    vec_data[0]
                } else {
                    *spec
                }
            } else {
                *spec
            }
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

    Ok(Some((
        func,
        default_call_interactively_args_in_state(
            &shared.obarray,
            &[],
            &shared.buffers,
            &shared.frames,
            &plan.resolved_name,
        )?,
    )))
}

pub(crate) fn resolve_call_interactively_target_and_args_with_vm_fallback(
    shared: &mut super::eval::Context,
    plan: &mut CallInteractivelyPlan,
    vm_gc_roots: &[Value],
    extra_roots: &[Value],
) -> Result<(Value, Vec<Value>), Flow> {
    if let Some((function, call_args)) =
        resolve_call_interactively_target_and_args_in_vm_runtime(shared, plan, vm_gc_roots)?
    {
        return Ok((function, call_args));
    }

    shared.with_extra_gc_roots(vm_gc_roots, extra_roots, move |eval| {
        resolve_call_interactively_target_and_args_in_eval(eval, plan)
    })
}

fn last_command_event_char(eval: &Context) -> Option<char> {
    let event = dynamic_or_global_symbol_value(eval, "last-command-event")?;
    match event.kind() {
        ValueKind::Char(c) => Some(c),
        ValueKind::Fixnum(n) if n >= 0 => char::from_u32(n as u32),
        _ => None,
    }
}

/// `(self-insert-command N &optional NOAUTOFILL)` -- insert the last typed
/// character N times.
pub(crate) fn builtin_self_insert_command(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("self-insert-command"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let repeats = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
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
        return Ok(Value::NIL);
    }
    if args.get(1).is_some_and(|v| !v.is_nil()) {
        return Ok(Value::NIL);
    }

    let Some(ch) = last_command_event_char(eval) else {
        return Ok(Value::NIL);
    };
    let Some(repeat_count) = usize::try_from(repeats).ok() else {
        return Ok(Value::NIL);
    };
    let mut text = String::new();
    for _ in 0..repeat_count {
        text.push(ch);
    }
    if let Some(current_id) = eval.buffers.current_buffer_id() {
        let insert_pos = eval.buffers.get(current_id).map(|b| b.pt).unwrap_or(0);
        let text_len = text.len();
        super::editfns::signal_before_change(eval, insert_pos, insert_pos)?;
        let _ = eval.buffers.insert_into_buffer(current_id, &text);
        super::editfns::signal_after_change(eval, insert_pos, insert_pos + text_len, 0)?;
    }
    Ok(Value::NIL)
}

/// `(keyboard-quit)` -- cancel the current command sequence.
pub(crate) fn builtin_keyboard_quit(_eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_args("keyboard-quit", &args, 0)?;
    Err(signal("quit", vec![]))
}

/// `(key-binding KEY &optional ACCEPT-DEFAULTS NO-REMAP POSITION)`
/// Return the binding for KEY in the current keymaps.
pub(crate) fn builtin_key_binding(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_key_binding_impl(eval, args)
}

pub(crate) fn builtin_key_binding_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("key-binding", &args, 1)?;
    expect_max_args("key-binding", &args, 4)?;
    let string_designator = args[0].is_string();
    let no_remap = args.get(2).is_some_and(|v| v.is_truthy());
    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(super::kbd::KeyDesignatorError::WrongType(other)) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), other],
            ));
        }
        Err(super::kbd::KeyDesignatorError::Parse(_)) => {
            return Ok(Value::NIL);
        }
    };
    if events.is_empty() {
        if !string_designator {
            return Ok(Value::NIL);
        }
        let active_maps = current_active_maps_for_position(ctx, true, args.get(3))?;
        return Ok(Value::list(active_maps));
    }

    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();
    let accept_default = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(
        resolve_active_key_binding(ctx, &emacs_events, accept_default, no_remap, args.get(3))?
            .binding,
    )
}

/// `(local-key-binding KEY &optional ACCEPT-DEFAULTS)`
pub(crate) fn builtin_local_key_binding(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_local_key_binding_impl(eval, args)
}

pub(crate) fn builtin_local_key_binding_impl(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("local-key-binding", &args, 1)?;
    expect_max_args("local-key-binding", &args, 2)?;

    if ctx.buffers.current_local_map().is_nil() {
        return Ok(Value::NIL);
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
            return Ok(Value::NIL);
        }
    };
    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();
    Ok(lookup_keymap_with_partial(
        &ctx.buffers.current_local_map(),
        &emacs_events,
    ))
}

/// `(minor-mode-key-binding KEY &optional ACCEPT-DEFAULTS)`
/// Look up KEY in active minor mode keymaps.
pub(crate) fn builtin_minor_mode_key_binding(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_minor_mode_key_binding_impl(eval, args)
}

pub(crate) fn builtin_minor_mode_key_binding_impl(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("minor-mode-key-binding", &args, 1)?;
    expect_max_args("minor-mode-key-binding", &args, 2)?;

    // Emacs returns nil (not a type error) for non-array key designators here.
    let events = match super::kbd::key_events_from_designator(&args[0]) {
        Ok(events) => events,
        Err(_) => return Ok(Value::NIL),
    };
    let emacs_events: Vec<Value> = events.iter().map(key_event_to_emacs_event).collect();
    minor_mode_key_binding_in_context(ctx, &emacs_events)
}

/// `(where-is-internal DEFINITION &optional KEYMAP FIRSTONLY NOINDIRECT NO-REMAP)`
/// Return list of key sequences that invoke DEFINITION.
pub(crate) fn builtin_where_is_internal(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("where-is-internal", &args, 1)?;
    expect_max_args("where-is-internal", &args, 5)?;

    let definition = &args[0];
    let first_only = args.get(2).is_some_and(|v| !v.is_nil());

    let keymaps = where_is_keymaps_in_context(eval, args.get(1))?;
    if args.get(1).is_none() && keymaps.is_empty() {
        return Ok(Value::NIL);
    }

    let mut sequences = Vec::new();
    for keymap in &keymaps {
        let mut prefix = Vec::new();
        if collect_where_is_sequences_value(
            eval.obarray(),
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
        return Ok(Value::NIL);
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
pub(crate) fn builtin_this_command_keys(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_this_command_keys_impl(eval.read_command_keys(), args)
}

pub(crate) fn builtin_this_command_keys_impl(
    read_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-command-keys", &args, 0)?;
    if !read_command_keys.is_empty() {
        return Ok(make_event_array_value(read_command_keys));
    }
    Ok(Value::string(""))
}

/// `(this-command-keys-vector)` -> vector of keys that invoked current command.
pub(crate) fn builtin_this_command_keys_vector(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_this_command_keys_vector_impl(eval.read_command_keys(), args)
}

pub(crate) fn builtin_this_command_keys_vector_impl(
    read_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-command-keys-vector", &args, 0)?;
    if !read_command_keys.is_empty() {
        return Ok(Value::vector(read_command_keys.to_vec()));
    }
    Ok(Value::vector(Vec::<Value>::new()))
}

fn single_command_key_vector_in_state(read_command_keys: &[Value]) -> Value {
    if !read_command_keys.is_empty() {
        return Value::vector(read_command_keys.to_vec());
    }
    Value::vector(Vec::<Value>::new())
}

fn single_command_key_vector(eval: &Context) -> Value {
    single_command_key_vector_in_state(eval.read_command_keys())
}

pub(crate) fn builtin_this_single_command_keys_impl(
    read_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-single-command-keys", &args, 0)?;
    Ok(single_command_key_vector_in_state(read_command_keys))
}

pub(crate) fn builtin_this_single_command_raw_keys_impl(
    read_raw_command_keys: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("this-single-command-raw-keys", &args, 0)?;
    Ok(single_command_key_vector_in_state(read_raw_command_keys))
}

/// `(this-single-command-keys)` -> vector of keys that invoked current command.
pub(crate) fn builtin_this_single_command_keys(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_this_single_command_keys_impl(eval.read_command_keys(), args)
}

/// `(this-single-command-raw-keys)` -> vector of raw keys for current command.
pub(crate) fn builtin_this_single_command_raw_keys(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_this_single_command_raw_keys_impl(eval.read_raw_command_keys(), args)
}

/// `(clear-this-command-keys &optional KEEP-RECORD)` -> nil.
///
/// Clears current command-key context used by `this-command-keys*`.
/// When KEEP-RECORD is nil or omitted, also clears recent input history used
/// by `recent-keys`.
pub(crate) fn builtin_clear_this_command_keys(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    builtin_clear_this_command_keys_in_runtime(eval, args)
}

pub(crate) trait CommandKeyRuntime {
    fn read_command_keys(&self) -> &[Value];
    fn clear_command_key_state(&mut self, keep_record: bool);
}

impl CommandKeyRuntime for Context {
    fn read_command_keys(&self) -> &[Value] {
        Context::read_command_keys(self)
    }

    fn clear_command_key_state(&mut self, keep_record: bool) {
        Context::clear_command_key_state(self, keep_record);
    }
}

pub(crate) fn builtin_clear_this_command_keys_in_runtime(
    runtime: &mut impl CommandKeyRuntime,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("clear-this-command-keys", &args, 1)?;
    let keep_record = args.first().is_some_and(|arg| arg.is_truthy());
    runtime.clear_command_key_state(keep_record);
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn command_remapping_keymap_arg_valid(value: &Value) -> bool {
    // Oracle accepts cons/list keymap-like objects in this slot, not just valid keymaps.
    // Non-keymap cons cells are silently treated as "no remap found".
    value.is_cons() || is_list_keymap(value)
}

fn command_remapping_lookup_in_keymaps(keymaps: &[Value], command_name: &str) -> Option<Value> {
    keymap_command_remapping_lookup_in_keymaps(keymaps, command_name)
}

fn command_remapping_command_name(command: &Value) -> Option<String> {
    keymap_command_remapping_command_name(command)
}

fn command_remapping_lookup_in_lisp_keymap(keymap: &Value, command_name: &str) -> Option<Value> {
    keymap_command_remapping_lookup_in_lisp_keymap(keymap, command_name)
}

fn command_remapping_normalize_target(raw: Value) -> Value {
    keymap_command_remapping_normalize_target(raw)
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
    if let (Some(bid), Some(did)) = (binding.as_subr_id(), definition.as_subr_id()) {
        return bid == did;
    }
    // Check if binding is a symbol matching a Subr definition name
    if let Some(bname) = binding.as_symbol_name() {
        if let Some(id) = definition.as_subr_id() {
            return bname == resolve_sym(id);
        }
    }
    binding == definition
}

fn collect_where_is_sequences_value(
    obarray: &Obarray,
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
        if let Some(prefix_keymap) = where_is_binding_prefix_keymap(obarray, &binding) {
            if collect_where_is_sequences_value(
                obarray,
                &prefix_keymap,
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
        if collect_where_is_sequences_value(
            obarray,
            &parent,
            definition,
            prefix,
            out,
            first_only,
            depth + 1,
        ) {
            return true;
        }
    }

    false
}

fn where_is_binding_prefix_keymap(obarray: &Obarray, binding: &Value) -> Option<Value> {
    if is_list_keymap(binding) {
        return Some(*binding);
    }

    let sym_name = binding.as_symbol_name()?;
    let func = obarray.indirect_function(sym_name)?;
    if is_list_keymap(&func) {
        Some(func)
    } else {
        None
    }
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
