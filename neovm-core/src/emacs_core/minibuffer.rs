//! Minibuffer and completion system.
//!
//! Provides:
//! - `MinibufferManager` — owns all minibuffer state, history, and completion logic
//! - `CompletionTable` — what can be completed against (list, function, file names, etc.)
//! - `CompletionStyle` — matching strategy (prefix, substring, flex, basic)
//! - Builtin functions for Elisp: `read-from-minibuffer`, `completing-read`, `y-or-n-p`, etc.

use std::collections::HashMap;

use crate::buffer::{BufferId, BufferManager};

use super::error::{EvalResult, Flow, signal};
use super::hashtab::hash_key_to_visible_value;
use super::intern::resolve_sym;
use super::reader::KeyboardInputRuntime;
use super::symbol::Obarray;
use super::value::{Value, ValueKind, VecLikeType};

// ---------------------------------------------------------------------------
// Argument helpers (local copies, same pattern as builtins.rs / builtins_extra.rs)
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

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*val)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_lisp_string(value: &Value) -> Result<crate::heap_types::LispString, Flow> {
    value.as_lisp_string().cloned().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )
    })
}

fn first_default_value(default: Value) -> Value {
    match default.kind() {
        ValueKind::Cons => default.cons_car(),
        other => default,
    }
}

fn normalize_symbol_reader_default(default: Value) -> Value {
    match first_default_value(default).kind() {
        ValueKind::Symbol(id) => Value::string(resolve_sym(id)),
        other => first_default_value(default),
    }
}

fn normalize_buffer_reader_default(buffers: &BufferManager, default: Value) -> Value {
    let first = first_default_value(default);
    match first.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => first
            .as_buffer_id()
            .and_then(|id| buffers.get(id))
            .map(|buffer| Value::string(&buffer.name))
            .unwrap_or(first),
        _ => first,
    }
}

// ---------------------------------------------------------------------------
// CompletionTable
// ---------------------------------------------------------------------------

/// What can be completed against.
pub enum CompletionTable {
    /// Fixed list of completion candidates.
    List(Vec<String>),
    /// Dynamic completion function: given the current input, returns matching candidates.
    Function(Box<dyn Fn(&str) -> Vec<String>>),
    /// File name completion rooted at a directory.
    FileNames { directory: String },
    /// Buffer name completion (candidates supplied externally).
    BufferNames,
    /// Symbol name completion (candidates supplied externally).
    SymbolNames,
    /// Association list: each entry is (key, value).
    Alist(Vec<(String, Value)>),
}

impl CompletionTable {
    /// Extract the raw string candidates from the table.
    ///
    /// For `Function` tables the `input` is passed through; for static tables it
    /// is ignored (filtering happens later in the matching functions).
    fn candidates(&self, input: &str) -> Vec<String> {
        match self {
            CompletionTable::List(v) => v.clone(),
            CompletionTable::Function(f) => f(input),
            CompletionTable::FileNames { directory } => list_files_in_dir(directory),
            CompletionTable::BufferNames => Vec::new(),
            CompletionTable::SymbolNames => Vec::new(),
            CompletionTable::Alist(pairs) => pairs.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

/// Best-effort listing of file names in `dir`.  Returns an empty vec on I/O error.
fn list_files_in_dir(dir: &str) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// CompletionStyle
// ---------------------------------------------------------------------------

/// Matching strategy for completions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStyle {
    /// Standard prefix matching (case-insensitive).
    Prefix,
    /// Match anywhere in the candidate string.
    Substring,
    /// Fuzzy / flex matching: input characters must appear in order.
    Flex,
    /// Exact prefix (case-sensitive).
    Basic,
}

// ---------------------------------------------------------------------------
// CompletionResult
// ---------------------------------------------------------------------------

/// Result of a completion attempt.
pub struct CompletionResult {
    /// The candidates that matched.
    pub matches: Vec<String>,
    /// Longest common prefix of all matches (if any).
    pub common_prefix: Option<String>,
    /// Whether the match list is exhaustive (i.e. we know there are no more).
    pub exhaustive: bool,
}

// ---------------------------------------------------------------------------
// MinibufferState
// ---------------------------------------------------------------------------

/// Tracks one active minibuffer interaction (possibly recursive).
pub struct MinibufferState {
    pub buffer_id: BufferId,
    pub prompt: String,
    pub prompt_end: usize,
    pub initial_input: String,
    pub history: Vec<String>,
    pub history_position: Option<usize>,
    pub content: String,
    pub cursor_pos: usize,
    pub completion_table: Option<CompletionTable>,
    /// The `require-match` argument from `completing-read`.
    ///
    /// Possible semantic values:
    /// - `nil` — no restriction, any input accepted
    /// - `t` (or any non-nil, non-`confirm`, non-`confirm-after-completion`)
    ///   — must match exactly
    /// - symbol `confirm` — may exit with non-match after a second RET
    /// - symbol `confirm-after-completion` — like `confirm` but only after
    ///   the user has triggered a completion at least once
    pub require_match: Value,
    pub default_value: Option<String>,
    pub active: bool,
    /// Recursive minibuffer depth at which this state was entered.
    pub depth: usize,
    /// Command-loop depth active when this minibuffer was entered.
    pub command_loop_depth: usize,
}

impl MinibufferState {
    fn new(buffer_id: BufferId, prompt: String, initial: String, depth: usize) -> Self {
        let cursor_pos = initial.len();
        let prompt_end = prompt.len();
        Self {
            buffer_id,
            prompt,
            prompt_end,
            initial_input: initial.clone(),
            history: Vec::new(),
            history_position: None,
            content: initial,
            cursor_pos,
            completion_table: None,
            require_match: Value::NIL,
            default_value: None,
            active: true,
            depth,
            command_loop_depth: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// MinibufferHistory
// ---------------------------------------------------------------------------

/// Named history lists (e.g. "minibuffer-history", "file-name-history", ...).
pub struct MinibufferHistory {
    histories: HashMap<String, Vec<String>>,
}

impl MinibufferHistory {
    pub fn new() -> Self {
        Self {
            histories: HashMap::new(),
        }
    }

    pub fn get(&self, name: &str) -> &[String] {
        match self.histories.get(name) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Add a value to a named history list.
    ///
    /// `max_length` controls how many entries to keep.  Callers that have
    /// access to the obarray should read the `history-length` symbol and
    /// pass it here; the default in GNU Emacs is 100.
    pub fn add(&mut self, name: &str, value: &str, max_length: usize) {
        let list = self.histories.entry(name.to_string()).or_default();
        // Avoid consecutive duplicates at the front.
        if list.first().map(|s| s.as_str()) != Some(value) {
            list.insert(0, value.to_string());
        }
        if list.len() > max_length {
            list.truncate(max_length);
        }
    }
}

impl Default for MinibufferHistory {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MinibufferManager
// ---------------------------------------------------------------------------

/// Owns all minibuffer state, including the recursive-edit stack.
pub struct MinibufferManager {
    state_stack: Vec<MinibufferState>,
    history: MinibufferHistory,
    completion_style: CompletionStyle,
    enable_recursive: bool,
    #[cfg(test)]
    max_depth: usize,
}

impl MinibufferManager {
    pub fn new() -> Self {
        Self {
            state_stack: Vec::new(),
            history: MinibufferHistory::new(),
            completion_style: CompletionStyle::Prefix,
            enable_recursive: true,
            #[cfg(test)]
            max_depth: 10,
        }
    }

    /// Enter the minibuffer with the given prompt and optional initial input / history name.
    ///
    /// Returns a fresh `MinibufferState` that has been pushed onto the stack.
    /// The caller can further configure it (completion table, require-match, default).
    pub(crate) fn read_from_minibuffer(
        &mut self,
        buffer_id: BufferId,
        prompt: &str,
        initial: Option<&str>,
        history_name: Option<&str>,
    ) -> Result<&mut MinibufferState, Flow> {
        let new_depth = self.state_stack.len() + 1;
        #[cfg(test)]
        if new_depth > self.max_depth {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Command attempted to use minibuffer while in minibuffer",
                )],
            ));
        }
        if !self.enable_recursive && !self.state_stack.is_empty() {
            return Err(signal(
                "error",
                vec![Value::string(
                    "Command attempted to use minibuffer while in minibuffer",
                )],
            ));
        }

        let initial_str = initial.unwrap_or("").to_string();
        let mut state = MinibufferState::new(buffer_id, prompt.to_string(), initial_str, new_depth);

        // Pre-populate history from the named list.
        if let Some(name) = history_name {
            state.history = self.history.get(name).to_vec();
        }

        self.state_stack.push(state);
        // Safety: we just pushed, so unwrap is fine.
        Ok(self.state_stack.last_mut().unwrap())
    }

    /// Attempt to complete the current minibuffer content.
    pub fn try_complete(&self, state: &MinibufferState) -> CompletionResult {
        match &state.completion_table {
            Some(table) => {
                let input = &state.content;
                let matches = self.all_completions(input, table);
                let common = compute_common_prefix(&matches);
                let exhaustive = !matches!(table, CompletionTable::Function(_));
                CompletionResult {
                    matches,
                    common_prefix: common,
                    exhaustive,
                }
            }
            None => CompletionResult {
                matches: Vec::new(),
                common_prefix: None,
                exhaustive: true,
            },
        }
    }

    /// Return all completions of `prefix` against `table`.
    pub fn all_completions(&self, prefix: &str, table: &CompletionTable) -> Vec<String> {
        let candidates = table.candidates(prefix);
        match self.completion_style {
            CompletionStyle::Prefix => prefix_match(prefix, &candidates),
            CompletionStyle::Substring => substring_match(prefix, &candidates),
            CompletionStyle::Flex => flex_match(prefix, &candidates),
            CompletionStyle::Basic => basic_match(prefix, &candidates),
        }
    }

    /// Try to complete `prefix` to the longest common prefix of all matches.
    /// Returns `None` if there are no matches.
    pub fn try_completion_string(&self, prefix: &str, table: &CompletionTable) -> Option<String> {
        let matches = self.all_completions(prefix, table);
        compute_common_prefix(&matches)
    }

    /// Test whether `string` is an exact match in `table`.
    pub fn test_completion(&self, string: &str, table: &CompletionTable) -> bool {
        let candidates = table.candidates(string);
        candidates.iter().any(|c| c == string)
    }

    /// Exit the current minibuffer, returning its content (or the default if empty).
    pub fn exit_minibuffer(&mut self) -> Option<String> {
        if let Some(mut state) = self.state_stack.pop() {
            state.active = false;
            let result = if state.content.is_empty() {
                state.default_value.unwrap_or_default()
            } else {
                state.content.clone()
            };
            Some(result)
        } else {
            None
        }
    }

    /// Abort the current minibuffer (like C-g).
    pub fn abort_minibuffer(&mut self) {
        if let Some(mut state) = self.state_stack.pop() {
            state.active = false;
        }
    }

    /// Navigate to the previous (older) history entry.
    pub fn history_previous(&mut self) -> Option<String> {
        let state = self.state_stack.last_mut()?;
        let history = &state.history;
        if history.is_empty() {
            return None;
        }
        let new_pos = match state.history_position {
            None => 0,
            Some(p) => {
                if p + 1 < history.len() {
                    p + 1
                } else {
                    return None; // already at oldest
                }
            }
        };
        state.history_position = Some(new_pos);
        let entry = history[new_pos].clone();
        state.content = entry.clone();
        state.cursor_pos = entry.len();
        Some(entry)
    }

    /// Navigate to the next (newer) history entry.
    pub fn history_next(&mut self) -> Option<String> {
        let state = self.state_stack.last_mut()?;
        match state.history_position {
            None => None,
            Some(0) => {
                // Back to the original input.
                state.history_position = None;
                state.content = state.initial_input.clone();
                state.cursor_pos = state.initial_input.len();
                Some(state.initial_input.clone())
            }
            Some(p) => {
                let new_pos = p - 1;
                state.history_position = Some(new_pos);
                let entry = state.history[new_pos].clone();
                state.content = entry.clone();
                state.cursor_pos = entry.len();
                Some(entry)
            }
        }
    }

    /// Add a value to a named history list.
    ///
    /// `max_length` controls how many entries to keep.  Callers should read
    /// the `history-length` symbol from the obarray (default 100).
    pub fn add_to_history(&mut self, name: &str, value: &str, max_length: usize) {
        self.history.add(name, value, max_length);
    }

    /// Read the effective `history-length` from the obarray, defaulting to 100.
    pub fn history_length_from_obarray(obarray: &Obarray) -> usize {
        match obarray.symbol_value("history-length") {
            Some(v) if v.is_fixnum() && v.xfixnum() > 0 => v.xfixnum() as usize,
            _ => 100,
        }
    }

    /// Reference to the current (innermost) minibuffer state, if any.
    pub fn current(&self) -> Option<&MinibufferState> {
        self.state_stack.last()
    }

    /// Mutable reference to the current (innermost) minibuffer state.
    pub fn current_mut(&mut self) -> Option<&mut MinibufferState> {
        self.state_stack.last_mut()
    }

    /// Current recursive minibuffer depth (0 = not in minibuffer).
    pub fn depth(&self) -> usize {
        self.state_stack.len()
    }

    /// Whether any minibuffer is currently active.
    pub fn is_active(&self) -> bool {
        self.state_stack.last().is_some_and(|s| s.active)
    }

    pub fn has_buffer(&self, buffer_id: BufferId) -> bool {
        self.state_stack
            .iter()
            .any(|state| state.buffer_id == buffer_id)
    }

    /// Set the completion style.
    pub fn set_completion_style(&mut self, style: CompletionStyle) {
        self.completion_style = style;
    }

    /// Set whether recursive minibuffers are allowed.
    pub fn set_enable_recursive(&mut self, enable: bool) {
        self.enable_recursive = enable;
    }
}

impl Default for MinibufferManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Completion matching functions
// ---------------------------------------------------------------------------

/// Case-insensitive prefix matching.
fn prefix_match(input: &str, candidates: &[String]) -> Vec<String> {
    let lower_input = input.to_lowercase();
    candidates
        .iter()
        .filter(|c| c.to_lowercase().starts_with(&lower_input))
        .cloned()
        .collect()
}

/// Substring matching (case-insensitive).
fn substring_match(input: &str, candidates: &[String]) -> Vec<String> {
    let lower_input = input.to_lowercase();
    candidates
        .iter()
        .filter(|c| c.to_lowercase().contains(&lower_input))
        .cloned()
        .collect()
}

/// Flex (fuzzy) matching: the input characters must appear in order within the candidate.
fn flex_match(input: &str, candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| is_flex_match(input, c))
        .cloned()
        .collect()
}

/// Check if all characters in `input` appear in order in `candidate` (case-insensitive).
fn is_flex_match(input: &str, candidate: &str) -> bool {
    let mut chars = candidate.chars().flat_map(|c| c.to_lowercase());
    for ic in input.chars().flat_map(|c| c.to_lowercase()) {
        loop {
            match chars.next() {
                Some(cc) if cc == ic => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

/// Exact (case-sensitive) prefix matching.
fn basic_match(input: &str, candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| c.starts_with(input))
        .cloned()
        .collect()
}

/// Compute the longest common prefix of a set of strings.
/// Returns `None` if the set is empty.
fn compute_common_prefix(strings: &[String]) -> Option<String> {
    if strings.is_empty() {
        return None;
    }
    let first = &strings[0];
    let mut prefix_len = first.len();
    for s in &strings[1..] {
        prefix_len = first
            .chars()
            .zip(s.chars())
            .take(prefix_len)
            .take_while(|(a, b)| a == b)
            .count();
        if prefix_len == 0 {
            return Some(String::new());
        }
    }
    // `prefix_len` is in *chars*; collect the first `prefix_len` chars.
    Some(first.chars().take(prefix_len).collect())
}

// ---------------------------------------------------------------------------
// Builtin functions for Elisp
// ---------------------------------------------------------------------------

/// `(read-file-name PROMPT &optional DIR DEFAULT MUSTMATCH INITIAL PREDICATE)`
///
/// Read a file name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer with initial directory context.
pub(crate) fn builtin_read_file_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_file_name_in_runtime(eval, &args)?;
    finish_read_file_name_in_eval(eval, &args)
}

/// `(read-directory-name PROMPT &optional DIR DEFAULT MUSTMATCH INITIAL)`
///
/// Read a directory name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer with initial directory context.
pub(crate) fn builtin_read_directory_name(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_directory_name_in_runtime(eval, &args)?;
    finish_read_directory_name_in_eval(eval, &args)
}

fn validate_file_name_reader_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    expect_min_args(name, args, 1)?;
    expect_max_args(name, args, max)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(dir) = args.get(1)
        && !dir.is_nil()
    {
        let _ = expect_string(dir)?;
    }
    if let Some(default) = args.get(2)
        && !default.is_nil()
    {
        let _ = expect_string(default)?;
    }
    if let Some(initial) = args.get(4)
        && !initial.is_nil()
    {
        let _ = expect_string(initial)?;
    }
    Ok(())
}

fn file_name_reader_minibuffer_args(args: &[Value]) -> [Value; 6] {
    let prompt = args[0];
    let initial = args.get(4).copied().unwrap_or(Value::NIL);
    let default = args.get(2).copied().unwrap_or(Value::NIL);
    let effective_initial = if initial.is_nil() {
        args.get(1).copied().unwrap_or(Value::NIL)
    } else {
        initial
    };
    [
        prompt,
        effective_initial,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        default,
    ]
}

pub(crate) fn builtin_read_file_name_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    validate_file_name_reader_args("read-file-name", args, 6)?;
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(end_of_file_stdin_error())
    }
}

pub(crate) fn finish_read_file_name_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = file_name_reader_minibuffer_args(args);
    read_from_minibuffer(&minibuffer_args)
}

pub(crate) fn finish_read_file_name_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_file_name_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn finish_read_file_name_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_file_name_in_runtime(shared, args)?;
    finish_read_file_name_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_vm_runtime(
            shared,
            vm_gc_roots,
            minibuffer_args,
        )
    })
}

pub(crate) fn builtin_read_directory_name_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    validate_file_name_reader_args("read-directory-name", args, 5)?;
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(end_of_file_stdin_error())
    }
}

pub(crate) fn finish_read_directory_name_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_file_name_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn finish_read_directory_name_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_directory_name_in_runtime(shared, args)?;
    finish_read_file_name_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_vm_runtime(
            shared,
            vm_gc_roots,
            minibuffer_args,
        )
    })
}

/// `(read-buffer PROMPT &optional DEFAULT REQUIRE-MATCH PREDICATE)`
///
/// Read a buffer name from the minibuffer with completion.
/// In interactive mode, delegates to completing-read with buffer name candidates.
pub(crate) fn builtin_read_buffer(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_read_buffer_in_runtime(eval, &args)?;
    finish_read_buffer_in_eval(eval, &args)
}

pub(crate) fn finish_read_buffer_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    let completing_args = read_buffer_completing_args(eval.buffer_manager(), args);
    super::reader::finish_completing_read_in_eval(eval, &completing_args)
}

pub(crate) fn builtin_read_buffer_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-buffer", args, 1)?;
    expect_max_args("read-buffer", args, 4)?;
    let _prompt = expect_string(&args[0])?;
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(end_of_file_stdin_error())
    }
}

pub(crate) fn read_buffer_completing_args(buffers: &BufferManager, args: &[Value]) -> [Value; 7] {
    let prompt = args[0];
    let default =
        normalize_buffer_reader_default(buffers, args.get(1).copied().unwrap_or(Value::NIL));
    let require_match = args.get(2).copied().unwrap_or(Value::NIL);
    let predicate = args.get(3).copied().unwrap_or(Value::NIL);

    let buf_ids = buffers.buffer_list();
    let buffer_names: Vec<Value> = buf_ids
        .iter()
        .filter_map(|id| buffers.get(*id))
        .map(|b| Value::string(&b.name))
        .collect();
    let collection = Value::list(buffer_names);

    [
        prompt,
        collection,
        predicate,
        require_match,
        Value::NIL,
        Value::NIL,
        default,
    ]
}

/// `(read-command PROMPT &optional DEFAULT)`
///
/// Read a command name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer and interns the result.
pub(crate) fn builtin_read_command(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_command_in_runtime(eval, &args)?;
    finish_read_command_in_eval(eval, &args)
}

pub(crate) fn finish_read_command_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_command_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn builtin_read_command_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-command", args, 1)?;
    expect_max_args("read-command", args, 2)?;
    let _prompt = expect_string(&args[0])?;
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(end_of_file_stdin_error())
    }
}

fn symbol_reader_minibuffer_args(args: &[Value]) -> [Value; 6] {
    let prompt = args[0];
    let default = normalize_symbol_reader_default(args.get(1).copied().unwrap_or(Value::NIL));
    [
        prompt,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        default,
    ]
}

fn intern_symbol_reader_result(result: Value) -> Value {
    if result.is_string() {
        let name = super::builtins::lisp_string_to_runtime_string(result);
        return Value::symbol(&name);
    }
    result
}

fn finish_symbol_reader_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = symbol_reader_minibuffer_args(args);
    let result = read_from_minibuffer(&minibuffer_args)?;
    Ok(intern_symbol_reader_result(result))
}

pub(crate) fn finish_read_command_with_minibuffer(
    args: &[Value],
    read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    finish_symbol_reader_with_minibuffer(args, read_from_minibuffer)
}

pub(crate) fn finish_read_command_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_command_in_runtime(shared, args)?;
    finish_read_command_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_vm_runtime(
            shared,
            vm_gc_roots,
            minibuffer_args,
        )
    })
}

/// `(read-variable PROMPT &optional DEFAULT)`
///
/// Read a variable name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer and interns the result.
pub(crate) fn builtin_read_variable(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_variable_in_runtime(eval, &args)?;
    finish_read_variable_in_eval(eval, &args)
}

pub(crate) fn finish_read_variable_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_variable_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn builtin_read_variable_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-variable", args, 1)?;
    expect_max_args("read-variable", args, 2)?;
    let _prompt = expect_string(&args[0])?;
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(end_of_file_stdin_error())
    }
}

pub(crate) fn finish_read_variable_with_minibuffer(
    args: &[Value],
    read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    finish_symbol_reader_with_minibuffer(args, read_from_minibuffer)
}

pub(crate) fn finish_read_variable_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_variable_in_runtime(shared, args)?;
    finish_read_variable_with_minibuffer(args, |minibuffer_args| {
        super::reader::finish_read_from_minibuffer_in_vm_runtime(
            shared,
            vm_gc_roots,
            minibuffer_args,
        )
    })
}

/// `(minibuffer-prompt)` — returns the current minibuffer prompt or nil.
///
/// Stub: returns nil (no active minibuffer in non-interactive mode).
pub(crate) fn builtin_minibuffer_prompt(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-prompt", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_minibuffer_prompt_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-prompt", &args, 0)?;
    Ok(eval
        .minibuffers
        .current()
        .map(|state| Value::string(&state.prompt))
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_minibuffer_prompt_end_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-prompt-end", &args, 0)?;

    let Some(buffer) = eval.buffers.current_buffer() else {
        return Ok(Value::fixnum(1));
    };
    let point_min = buffer.point_min_char() as i64 + 1;

    let Some(state) = eval.minibuffers.current() else {
        return Ok(Value::fixnum(point_min));
    };
    if state.buffer_id != buffer.id {
        return Ok(Value::fixnum(point_min));
    }

    let prompt_end_byte = state.prompt_end.min(buffer.text.len());
    let prompt_end_char = buffer.text.byte_to_char(prompt_end_byte) as i64 + 1;
    Ok(Value::fixnum(prompt_end_char.max(point_min)))
}

/// `(minibuffer-contents)` — returns the current minibuffer contents.
///
/// In non-interactive batch mode, Emacs exposes current buffer contents.
pub(crate) fn builtin_minibuffer_contents_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-contents", &args, 0)?;
    let text = minibuffer_contents_string(&eval.minibuffers, &eval.buffers);
    Ok(Value::string(text))
}

/// `(minibuffer-contents-no-properties)` — returns minibuffer contents
/// without text properties.
///
/// NeoVM stores plain strings for this path, so this is equivalent to
/// `minibuffer-contents` in batch mode.
pub(crate) fn builtin_minibuffer_contents_no_properties_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-contents-no-properties", &args, 0)?;
    let text = minibuffer_contents_string(&eval.minibuffers, &eval.buffers);
    Ok(Value::string(text))
}

/// `(minibuffer-depth)` — returns the current recursive minibuffer depth.
///
/// Stub: returns 0.
pub(crate) fn builtin_minibuffer_depth(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-depth", &args, 0)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_minibuffer_depth_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-depth", &args, 0)?;
    Ok(Value::fixnum(eval.minibuffers.depth() as i64))
}

/// `(minibufferp &optional BUFFER)` — returns t if BUFFER is a minibuffer.
///
/// Batch-compatible behavior: accepts 0..=2 args, validates BUFFER-like first
/// arg shape, and returns nil (no active minibuffer).
pub(crate) fn builtin_minibufferp(args: Vec<Value>) -> EvalResult {
    validate_minibufferp_args(&args)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_minibufferp_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    validate_minibufferp_args(&args)?;
    let live_only = args.get(1).is_some_and(|v| v.is_truthy());
    let Some(buffer_id) = resolve_minibuffer_buffer_arg(&eval.buffers, args.first())? else {
        return Ok(Value::NIL);
    };
    let is_live = eval.minibuffers.has_buffer(buffer_id);
    let is_minibuffer = is_live
        || eval
            .buffers
            .get(buffer_id)
            .is_some_and(|buffer| is_minibuffer_buffer_name(&buffer.name));
    Ok(Value::bool_val(if live_only {
        is_live
    } else {
        is_minibuffer
    }))
}

pub(crate) fn builtin_minibuffer_innermost_command_loop_p_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("minibuffer-innermost-command-loop-p", &args, 0, 1)?;
    let Some(buffer_id) = resolve_minibuffer_buffer_arg(&eval.buffers, args.first())? else {
        return Ok(Value::NIL);
    };
    let recursive_depth = eval.recursive_command_loop_depth();
    let command_loop_depth = eval
        .minibuffers
        .state_stack
        .iter()
        .find(|state| state.buffer_id == buffer_id)
        .map(|state| state.command_loop_depth);
    Ok(Value::bool_val(
        command_loop_depth.is_some_and(|depth| depth == recursive_depth),
    ))
}

pub(crate) fn builtin_innermost_minibuffer_p_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("innermost-minibuffer-p", &args, 0, 1)?;
    let Some(buffer_id) = resolve_minibuffer_buffer_arg(&eval.buffers, args.first())? else {
        return Ok(Value::NIL);
    };
    Ok(Value::bool_val(
        eval.minibuffers
            .current()
            .is_some_and(|state| state.buffer_id == buffer_id),
    ))
}

fn validate_minibufferp_args(args: &[Value]) -> Result<(), Flow> {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("minibufferp"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    if let Some(bufferish) = args.first() {
        match bufferish.kind() {
            ValueKind::Nil | ValueKind::String | ValueKind::Veclike(VecLikeType::Buffer) => {}
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("bufferp"), *bufferish],
                ));
            }
        }
    }
    Ok(())
}

/// Eval-aware `(recursive-edit)` — enters the command loop.
///
/// Mirrors GNU Emacs keyboard.c:772 `Frecursive_edit`.
pub(crate) fn builtin_recursive_edit(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("recursive-edit", &args, 0)?;
    eval.recursive_edit_inner()
}

/// `(top-level)` — exit all recursive edits.
///
/// Mirrors GNU Emacs keyboard.c:1187 `Ftop_level`.
/// Throws to the `top-level` tag to unwind all recursive edits.
pub(crate) fn builtin_top_level(args: Vec<Value>) -> EvalResult {
    expect_args("top-level", &args, 0)?;
    Err(Flow::Throw {
        tag: Value::symbol("top-level"),
        value: Value::NIL,
    })
}

/// `(exit-recursive-edit)` — exit innermost recursive edit.
///
/// Mirrors GNU Emacs keyboard.c:1211 `Fexit_recursive_edit`.
/// Throws to the `exit` tag with nil value.
pub(crate) fn builtin_exit_recursive_edit(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("exit-recursive-edit", &args, 0)?;
    // GNU Emacs checks: command_loop_level > 0 || minibuf_level > 0
    if eval.recursive_command_loop_depth() == 0 && eval.minibuffers.depth() == 0 {
        return Err(signal(
            "user-error",
            vec![Value::string("No recursive edit is in progress")],
        ));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::NIL,
    })
}

/// `(exit-minibuffer)` — exit the active minibuffer.
///
/// Emacs exits by throwing to the `exit` tag; without a catch this
/// surfaces as `no-catch`.
pub(crate) fn builtin_exit_minibuffer(args: Vec<Value>) -> EvalResult {
    expect_args("exit-minibuffer", &args, 0)?;
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::NIL,
    })
}

/// `(abort-minibuffers)` — abort active minibuffer sessions.
///
/// Batch/non-interactive mode has no active minibuffer, so this matches GNU
/// Emacs by signaling a plain `error`.
pub(crate) fn builtin_abort_minibuffers(args: Vec<Value>) -> EvalResult {
    expect_args("abort-minibuffers", &args, 0)?;
    Err(signal("error", vec![Value::string("Not in a minibuffer")]))
}

pub(crate) fn builtin_abort_minibuffers_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("abort-minibuffers", &args, 0)?;
    let in_active_minibuffer = eval
        .buffers
        .current_buffer_id()
        .is_some_and(|buffer_id| eval.minibuffers.has_buffer(buffer_id));
    if !in_active_minibuffer {
        return Err(signal("error", vec![Value::string("Not in a minibuffer")]));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::T,
    })
}

fn minibuffer_contents_string(minibuffers: &MinibufferManager, buffers: &BufferManager) -> String {
    let Some(buffer) = buffers.current_buffer() else {
        return String::new();
    };
    if let Some(state) = minibuffers.current()
        && state.buffer_id == buffer.id
    {
        return buffer.buffer_substring(state.prompt_end.min(buffer.text.len()), buffer.text.len());
    }
    buffer.buffer_string()
}

fn resolve_minibuffer_buffer_arg(
    buffers: &BufferManager,
    bufferish: Option<&Value>,
) -> Result<Option<BufferId>, Flow> {
    let Some(val) = bufferish else {
        return Ok(buffers.current_buffer_id());
    };
    match val.kind() {
        ValueKind::Nil => Ok(buffers.current_buffer_id()),
        ValueKind::Veclike(VecLikeType::Buffer) => Ok(val.as_buffer_id()),
        ValueKind::String => Ok(val
            .as_str()
            .and_then(|name| buffers.find_buffer_by_name(name))),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), *val],
        )),
    }
}

fn is_minibuffer_buffer_name(name: &str) -> bool {
    name.starts_with(" *Minibuf-") && name.ends_with('*')
}

/// `(abort-recursive-edit)` — abort the innermost recursive edit.
///
/// Mirrors GNU Emacs keyboard.c:1222 `Fabort_recursive_edit`.
/// Throws to the `exit` tag with `t` value (signals quit on catch).
pub(crate) fn builtin_abort_recursive_edit(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("abort-recursive-edit", &args, 0)?;
    // GNU Emacs checks: command_loop_level > 0 || minibuf_level > 0
    if eval.recursive_command_loop_depth() == 0 && eval.minibuffers.depth() == 0 {
        return Err(signal(
            "user-error",
            vec![Value::string("No recursive edit is in progress")],
        ));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::T,
    })
}

// ---------------------------------------------------------------------------
// Value-to-string-list conversion helper
// ---------------------------------------------------------------------------

/// Extract a list of strings from a Value.
///
/// Handles:
/// - Proper list of strings
/// - Alist of (string . _) pairs
/// - Vector of strings
/// - nil → empty
fn value_to_string_list(val: &Value) -> Vec<String> {
    match val.kind() {
        ValueKind::Nil => Vec::new(),
        ValueKind::Cons => {
            let items = match super::value::list_to_vec(val) {
                Some(v) => v,
                None => return Vec::new(),
            };
            items
                .iter()
                .filter_map(|item| match item.kind() {
                    ValueKind::String => completion_display_string_from_value(item),
                    ValueKind::Symbol(id) => Some(resolve_sym(id).to_owned()),
                    // Alist entry: (STRING . _)
                    ValueKind::Cons => {
                        let pair_car = item.cons_car();
                        completion_display_string_from_value(&pair_car)
                    }
                    _ => None,
                })
                .collect()
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let vec = val.as_vector_data().unwrap().clone();
            vec.iter()
                .filter_map(|item| match item.kind() {
                    ValueKind::String => completion_display_string_from_value(item),
                    ValueKind::Symbol(id) => Some(resolve_sym(id).to_owned()),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[derive(Clone)]
pub(crate) struct CompletionCandidate {
    completion: CompletionText,
    predicate_arg: Value,
    predicate_extra_arg: Option<Value>,
}

#[derive(Clone)]
enum CompletionText {
    OriginalString {
        value: Value,
        string: crate::heap_types::LispString,
    },
    Generated {
        string: crate::heap_types::LispString,
    },
}

impl CompletionText {
    fn lisp_string(&self) -> &crate::heap_types::LispString {
        match self {
            Self::OriginalString { string, .. } | Self::Generated { string } => string,
        }
    }

    fn as_result_value(&self) -> Value {
        match self {
            Self::OriginalString { value, .. } => *value,
            Self::Generated { string } => Value::heap_string(string.clone()),
        }
    }

    fn substring_value(&self, end_chars: usize) -> Value {
        match self {
            Self::OriginalString { value, string } if end_chars >= string.schars() => *value,
            _ => {
                let string = self.lisp_string();
                let end_chars = end_chars.min(string.schars());
                let end_byte = if string.is_multibyte() {
                    crate::emacs_core::emacs_char::char_to_byte_pos(string.as_bytes(), end_chars)
                } else {
                    end_chars.min(string.byte_len())
                };
                let sliced = string
                    .slice(0, end_byte)
                    .expect("validated completion prefix slice");
                Value::heap_string(sliced)
            }
        }
    }

    fn searched_string(&self) -> super::regex::SearchedString {
        match self {
            Self::OriginalString { value, .. } => super::regex::SearchedString::Heap(*value),
            Self::Generated { string } => super::regex::SearchedString::Owned(string.clone()),
        }
    }
}

fn completion_text_from_value(value: &Value) -> Option<CompletionText> {
    match value.kind() {
        ValueKind::String => {
            value
                .as_lisp_string()
                .cloned()
                .map(|string| CompletionText::OriginalString {
                    value: *value,
                    string,
                })
        }
        ValueKind::Symbol(id) => Some(CompletionText::Generated {
            string: crate::heap_types::LispString::from_utf8(resolve_sym(id)),
        }),
        ValueKind::Nil => Some(CompletionText::Generated {
            string: crate::heap_types::LispString::from_utf8("nil"),
        }),
        ValueKind::T => Some(CompletionText::Generated {
            string: crate::heap_types::LispString::from_utf8("t"),
        }),
        _ => None,
    }
}

fn completion_display_string_from_value(value: &Value) -> Option<String> {
    let completion = completion_text_from_value(value)?;
    let string = completion.lisp_string();
    Some(
        string
            .as_str()
            .map(|text| text.to_owned())
            .unwrap_or_else(|| crate::emacs_core::emacs_char::to_utf8_lossy(string.as_bytes())),
    )
}

fn completion_candidates_from_list_value(collection: &Value) -> Vec<CompletionCandidate> {
    let items = match super::value::list_to_vec(collection) {
        Some(items) => items,
        None => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let key = match item.kind() {
                ValueKind::Cons => item.cons_car(),
                other => item,
            };
            completion_text_from_value(&key).map(|completion| CompletionCandidate {
                completion,
                predicate_arg: item,
                predicate_extra_arg: None,
            })
        })
        .collect()
}

fn completion_candidates_from_vector_value(collection: &Value) -> Vec<CompletionCandidate> {
    let Some(items) = collection.as_vector_data() else {
        return Vec::new();
    };
    let items = items.clone();
    items
        .into_iter()
        .filter_map(|item| {
            completion_text_from_value(&item).map(|completion| CompletionCandidate {
                completion,
                predicate_arg: item,
                predicate_extra_arg: None,
            })
        })
        .collect()
}

fn completion_char_codes(string: &crate::heap_types::LispString) -> Vec<u32> {
    super::builtins::lisp_string_char_codes(string)
}

fn completion_fold_char(code: u32, ignore_case: bool) -> u32 {
    if !ignore_case {
        return code;
    }
    crate::emacs_core::builtins::downcase_char_code_emacs_compat(code as i64) as u32
}

fn completion_text_matches_prefix(
    prefix: &crate::heap_types::LispString,
    completion: &CompletionText,
    ignore_case: bool,
) -> bool {
    let prefix_codes = completion_char_codes(prefix);
    let completion_codes = completion_char_codes(completion.lisp_string());
    if prefix_codes.len() > completion_codes.len() {
        return false;
    }
    prefix_codes
        .iter()
        .zip(completion_codes.iter())
        .all(|(left, right)| {
            completion_fold_char(*left, ignore_case) == completion_fold_char(*right, ignore_case)
        })
}

fn completion_text_equals_string(
    completion: &CompletionText,
    string: &crate::heap_types::LispString,
    ignore_case: bool,
) -> bool {
    let left = completion_char_codes(completion.lisp_string());
    let right = completion_char_codes(string);
    left.len() == right.len()
        && left.iter().zip(right.iter()).all(|(l, r)| {
            completion_fold_char(*l, ignore_case) == completion_fold_char(*r, ignore_case)
        })
}

fn completion_common_prefix_len(matches: &[&CompletionCandidate], ignore_case: bool) -> usize {
    let Some((first, rest)) = matches.split_first() else {
        return 0;
    };
    let mut prefix = completion_char_codes(first.completion.lisp_string());
    for candidate in rest {
        let other = completion_char_codes(candidate.completion.lisp_string());
        let max = prefix.len().min(other.len());
        let mut common = 0;
        while common < max
            && completion_fold_char(prefix[common], ignore_case)
                == completion_fold_char(other[common], ignore_case)
        {
            common += 1;
        }
        prefix.truncate(common);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.len()
}

fn is_global_obarray_proxy_in_state(obarray: &Obarray, value: &Value) -> bool {
    obarray
        .symbol_value("obarray")
        .is_some_and(|proxy| *proxy == *value)
}

fn completion_candidates_from_global_obarray_in_state(
    obarray: &Obarray,
) -> Vec<CompletionCandidate> {
    let mut entries: Vec<(crate::heap_types::LispString, Value)> = obarray
        .global_member_ids()
        .map(|id| {
            (
                crate::emacs_core::intern::resolve_sym_lisp_string(id).clone(),
                Value::from_sym_id(id),
            )
        })
        .collect();
    entries.sort_by(|(left, _), (right, _)| {
        left.as_bytes()
            .cmp(right.as_bytes())
            .then(left.is_multibyte().cmp(&right.is_multibyte()))
    });
    entries.dedup_by(|(left_name, left_sym), (right_name, right_sym)| {
        left_name == right_name && left_sym.bits() == right_sym.bits()
    });
    entries
        .into_iter()
        .map(|(name, sym)| CompletionCandidate {
            completion: CompletionText::Generated { string: name },
            predicate_arg: sym,
            predicate_extra_arg: None,
        })
        .collect()
}

pub(crate) fn completion_candidates_from_collection_in_state(
    ctx: &crate::emacs_core::eval::Context,
    collection: &Value,
) -> Result<Option<Vec<CompletionCandidate>>, Flow> {
    let obarray = &ctx.obarray;
    Ok(match collection.kind() {
        ValueKind::Nil | ValueKind::Cons => Some(completion_candidates_from_list_value(collection)),
        ValueKind::Veclike(VecLikeType::HashTable) => {
            Some(completion_candidates_from_hash_table(*collection))
        }
        ValueKind::Veclike(VecLikeType::Vector)
            if is_global_obarray_proxy_in_state(obarray, collection) =>
        {
            Some(completion_candidates_from_global_obarray_in_state(obarray))
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            super::builtins::symbols::expect_obarray_vector_id(collection)?;
            Some(completion_candidates_from_custom_obarray(*collection))
        }
        _ => None,
    })
}

fn completion_candidates_from_collection(
    eval: &super::eval::Context,
    collection: &Value,
) -> Result<Option<Vec<CompletionCandidate>>, Flow> {
    completion_candidates_from_collection_in_state(eval, collection)
}

fn completion_predicate_matches_with(
    predicate: Value,
    candidate: &CompletionCandidate,
    mut apply: impl FnMut(Value, Vec<Value>) -> EvalResult,
) -> Result<bool, Flow> {
    if predicate.is_nil() {
        return Ok(true);
    }
    let result = match candidate.predicate_extra_arg {
        Some(extra) => apply(predicate, vec![candidate.predicate_arg, extra])?,
        None => apply(predicate, vec![candidate.predicate_arg])?,
    };
    Ok(result.is_truthy())
}

pub(crate) fn builtin_try_completion_with_candidates(
    args: &[Value],
    candidates: Option<Vec<CompletionCandidate>>,
    ignore_case: bool,
    regexps: &[crate::heap_types::LispString],
    mut apply: impl FnMut(Value, Vec<Value>) -> EvalResult,
) -> EvalResult {
    expect_min_args("try-completion", args, 2)?;
    expect_max_args("try-completion", args, 3)?;
    let string = expect_lisp_string(&args[0])?;
    let predicate = args.get(2).copied().unwrap_or(Value::NIL);
    let collection = args[1];

    let Some(candidates) = candidates else {
        return apply(collection, vec![args[0], predicate, Value::NIL]);
    };

    let mut matches = Vec::new();
    for candidate in &candidates {
        if !completion_text_matches_prefix(&string, &candidate.completion, ignore_case) {
            continue;
        }
        if !regexps.is_empty() && !matches_completion_regexps(&candidate.completion, regexps) {
            continue;
        }
        if completion_predicate_matches_with(predicate, candidate, &mut apply)? {
            matches.push(candidate);
        }
    }

    if matches.is_empty() {
        return Ok(Value::NIL);
    }
    if matches.len() == 1 && completion_text_equals_string(&matches[0].completion, &string, false) {
        return Ok(Value::T);
    }
    Ok(matches[0]
        .completion
        .substring_value(completion_common_prefix_len(&matches, ignore_case)))
}

pub(crate) fn builtin_all_completions_with_candidates(
    args: &[Value],
    candidates: Option<Vec<CompletionCandidate>>,
    ignore_case: bool,
    regexps: &[crate::heap_types::LispString],
    mut apply: impl FnMut(Value, Vec<Value>) -> EvalResult,
) -> EvalResult {
    expect_min_args("all-completions", args, 2)?;
    expect_max_args("all-completions", args, 4)?;
    let string = expect_lisp_string(&args[0])?;
    let predicate = args.get(2).copied().unwrap_or(Value::NIL);
    let collection = args[1];

    let Some(candidates) = candidates else {
        return apply(collection, vec![args[0], predicate, Value::T]);
    };

    // Two-pass approach: first filter candidates using the predicate
    // (which may trigger GC via apply), then create string Values.
    // This avoids holding unrooted Value strings across GC-triggering
    // predicate calls.
    let mut matching_completions: Vec<CompletionText> = Vec::new();
    for candidate in &candidates {
        if !completion_text_matches_prefix(&string, &candidate.completion, ignore_case) {
            continue;
        }
        if !regexps.is_empty() && !matches_completion_regexps(&candidate.completion, regexps) {
            continue;
        }
        if completion_predicate_matches_with(predicate, candidate, &mut apply)? {
            matching_completions.push(candidate.completion.clone());
        }
    }
    // Now create Values — no GC can trigger between creation and list building
    let matches: Vec<Value> = matching_completions
        .into_iter()
        .map(|completion| completion.as_result_value())
        .collect();
    Ok(Value::list(matches))
}

pub(crate) fn builtin_test_completion_with_candidates(
    args: &[Value],
    candidates: Option<Vec<CompletionCandidate>>,
    ignore_case: bool,
    regexps: &[crate::heap_types::LispString],
    mut apply: impl FnMut(Value, Vec<Value>) -> EvalResult,
) -> EvalResult {
    expect_min_args("test-completion", args, 2)?;
    expect_max_args("test-completion", args, 3)?;
    let string = expect_lisp_string(&args[0])?;
    let predicate = args.get(2).copied().unwrap_or(Value::NIL);
    let collection = args[1];

    let Some(candidates) = candidates else {
        return apply(
            collection,
            vec![args[0], predicate, Value::symbol("lambda")],
        );
    };

    for candidate in &candidates {
        if !completion_text_equals_string(&candidate.completion, &string, ignore_case) {
            continue;
        }
        if !regexps.is_empty() && !matches_completion_regexps(&candidate.completion, regexps) {
            continue;
        }
        if completion_predicate_matches_with(predicate, candidate, &mut apply)? {
            return Ok(Value::T);
        }
    }
    Ok(Value::NIL)
}

fn completion_candidates_from_custom_obarray(collection: Value) -> Vec<CompletionCandidate> {
    let slots = collection.as_vector_data().unwrap().clone();
    let mut candidates = Vec::new();
    for slot in slots {
        let mut current = slot;
        loop {
            match current.kind() {
                ValueKind::Nil => break,
                ValueKind::Cons => {
                    let pair_car = current.cons_car();
                    let pair_cdr = current.cons_cdr();
                    if let Some(completion) = completion_text_from_value(&pair_car) {
                        candidates.push(CompletionCandidate {
                            completion,
                            predicate_arg: pair_car,
                            predicate_extra_arg: None,
                        });
                    }
                    current = pair_cdr;
                }
                _ => break,
            }
        }
    }
    candidates
}

fn completion_candidates_from_hash_table(collection: Value) -> Vec<CompletionCandidate> {
    let table = collection.as_hash_table().unwrap().clone();
    let mut candidates = Vec::new();
    for key in &table.insertion_order {
        let Some(value) = table.data.get(key).copied() else {
            continue;
        };
        let visible_key = hash_key_to_visible_value(&table, key);
        if let Some(completion) = completion_text_from_value(&visible_key) {
            candidates.push(CompletionCandidate {
                completion,
                predicate_arg: visible_key,
                predicate_extra_arg: Some(value),
            });
        }
    }
    candidates
}

/// Read the `completion-ignore-case` symbol from the obarray.
fn completion_ignore_case(obarray: &Obarray) -> bool {
    obarray
        .symbol_value("completion-ignore-case")
        .is_some_and(|v| v.is_truthy())
}

/// Read `completion-regexp-list` from the obarray and return the list of
/// regex pattern strings.  Returns an empty vec when the variable is nil
/// or unset.
///
/// Public alias for use from the bytecode VM.
pub(crate) fn completion_regexp_list_from_obarray(obarray: &Obarray) -> Vec<String> {
    completion_regexp_lisp_list_from_obarray(obarray)
        .into_iter()
        .map(|regexp| {
            regexp
                .as_str()
                .map(|text| text.to_owned())
                .unwrap_or_else(|| crate::emacs_core::emacs_char::to_utf8_lossy(regexp.as_bytes()))
        })
        .collect()
}

pub(crate) fn completion_regexp_lisp_list_from_obarray(
    obarray: &Obarray,
) -> Vec<crate::heap_types::LispString> {
    let Some(val) = obarray.symbol_value("completion-regexp-list").copied() else {
        return Vec::new();
    };
    let Some(items) = super::value::list_to_vec(&val) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_lisp_string().cloned())
        .collect()
}

/// Return `true` when `candidate` matches **all** regexps in `regexps`.
///
/// Uses the Emacs regex engine (`string_match_full_with_case_fold`) so that
/// Emacs-style `\(...\)` patterns work correctly.
fn matches_completion_regexps(
    candidate: &CompletionText,
    regexps: &[crate::heap_types::LispString],
) -> bool {
    for re in regexps {
        let mut md = None;
        // case-fold = false: GNU Emacs uses case-fold-search for the
        // individual re_search, but completion-regexp-list traditionally
        // respects completion-ignore-case only for the prefix test, not
        // for the regexp filter.  We match GNU behaviour by not folding.
        match super::regex::string_match_full_with_case_fold_source_lisp_pattern_posix(
            re,
            candidate.lisp_string(),
            candidate.searched_string(),
            0,
            false,
            false,
            &mut md,
        ) {
            Ok(Some(_)) => {}  // matched — continue checking remaining regexps
            _ => return false, // no match or error — candidate rejected
        }
    }
    true
}

pub(crate) fn builtin_try_completion(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let candidates = completion_candidates_from_collection(eval, &args[1])?;
    let ignore_case = completion_ignore_case(&eval.obarray);
    let regexps = completion_regexp_lisp_list_from_obarray(&eval.obarray);
    builtin_try_completion_with_candidates(
        &args,
        candidates,
        ignore_case,
        &regexps,
        |function, call_args| eval.apply(function, call_args),
    )
}

pub(crate) fn builtin_all_completions(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let candidates = completion_candidates_from_collection(eval, &args[1])?;
    let ignore_case = completion_ignore_case(&eval.obarray);
    let regexps = completion_regexp_lisp_list_from_obarray(&eval.obarray);
    builtin_all_completions_with_candidates(
        &args,
        candidates,
        ignore_case,
        &regexps,
        |function, call_args| eval.apply(function, call_args),
    )
}

pub(crate) fn builtin_test_completion(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let candidates = completion_candidates_from_collection(eval, &args[1])?;
    let ignore_case = completion_ignore_case(&eval.obarray);
    let regexps = completion_regexp_lisp_list_from_obarray(&eval.obarray);
    builtin_test_completion_with_candidates(
        &args,
        candidates,
        ignore_case,
        &regexps,
        |function, call_args| eval.apply(function, call_args),
    )
}

fn end_of_file_stdin_error() -> Flow {
    signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "minibuffer_test.rs"]
mod tests;
