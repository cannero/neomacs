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
use super::intern::resolve_sym;
use super::value::{Value, read_cons, with_heap};

// ---------------------------------------------------------------------------
// Argument helpers (local copies, same pattern as builtins.rs / builtins_extra.rs)
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

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn first_default_value(default: Value) -> Value {
    match default {
        Value::Cons(cell) => read_cons(cell).car,
        other => other,
    }
}

fn normalize_symbol_reader_default(default: Value) -> Value {
    match first_default_value(default) {
        Value::Symbol(id) => Value::string(resolve_sym(id)),
        other => other,
    }
}

fn normalize_buffer_reader_default(buffers: &BufferManager, default: Value) -> Value {
    match first_default_value(default) {
        Value::Buffer(id) => buffers
            .get(id)
            .map(|buffer| Value::string(&buffer.name))
            .unwrap_or(Value::Buffer(id)),
        other => other,
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
    pub require_match: bool,
    pub default_value: Option<String>,
    pub active: bool,
    /// Recursive minibuffer depth at which this state was entered.
    pub depth: usize,
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
            require_match: false,
            default_value: None,
            active: true,
            depth,
        }
    }
}

// ---------------------------------------------------------------------------
// MinibufferHistory
// ---------------------------------------------------------------------------

/// Named history lists (e.g. "minibuffer-history", "file-name-history", ...).
pub struct MinibufferHistory {
    histories: HashMap<String, Vec<String>>,
    max_length: usize,
}

impl MinibufferHistory {
    pub fn new() -> Self {
        Self {
            histories: HashMap::new(),
            max_length: 100,
        }
    }

    pub fn get(&self, name: &str) -> &[String] {
        match self.histories.get(name) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    pub fn add(&mut self, name: &str, value: &str) {
        let list = self.histories.entry(name.to_string()).or_default();
        // Avoid consecutive duplicates at the front.
        if list.first().map(|s| s.as_str()) != Some(value) {
            list.insert(0, value.to_string());
        }
        if list.len() > self.max_length {
            list.truncate(self.max_length);
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
    pub fn add_to_history(&mut self, name: &str, value: &str) {
        self.history.add(name, value);
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-file-name", &args, 1)?;
    expect_max_args("read-file-name", &args, 6)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(dir) = args.get(1) {
        if !dir.is_nil() {
            let _ = expect_string(dir)?;
        }
    }
    if let Some(default) = args.get(2) {
        if !default.is_nil() {
            let _ = expect_string(default)?;
        }
    }
    if let Some(initial) = args.get(4) {
        if !initial.is_nil() {
            let _ = expect_string(initial)?;
        }
    }

    // Interactive mode: use read-from-minibuffer with initial input
    if eval.input_rx.is_some() {
        let prompt = args[0];
        let initial = args.get(4).copied().unwrap_or(Value::Nil);
        let default = args.get(2).copied().unwrap_or(Value::Nil);

        // If no initial input but DIR is provided, use DIR as initial
        let effective_initial = if initial.is_nil() {
            args.get(1).copied().unwrap_or(Value::Nil)
        } else {
            initial
        };

        return super::reader::builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                effective_initial,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                default,
            ],
        );
    }

    Err(end_of_file_stdin_error())
}

/// `(read-directory-name PROMPT &optional DIR DEFAULT MUSTMATCH INITIAL)`
///
/// Read a directory name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer with initial directory context.
pub(crate) fn builtin_read_directory_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-directory-name", &args, 1)?;
    expect_max_args("read-directory-name", &args, 5)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(dir) = args.get(1) {
        if !dir.is_nil() {
            let _ = expect_string(dir)?;
        }
    }
    if let Some(default) = args.get(2) {
        if !default.is_nil() {
            let _ = expect_string(default)?;
        }
    }
    if let Some(initial) = args.get(4) {
        if !initial.is_nil() {
            let _ = expect_string(initial)?;
        }
    }

    // Interactive mode: use read-from-minibuffer
    if eval.input_rx.is_some() {
        let prompt = args[0];
        let initial = args.get(4).copied().unwrap_or(Value::Nil);
        let default = args.get(2).copied().unwrap_or(Value::Nil);
        let effective_initial = if initial.is_nil() {
            args.get(1).copied().unwrap_or(Value::Nil)
        } else {
            initial
        };

        return super::reader::builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                effective_initial,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                default,
            ],
        );
    }

    Err(end_of_file_stdin_error())
}

/// `(read-buffer PROMPT &optional DEFAULT REQUIRE-MATCH PREDICATE)`
///
/// Read a buffer name from the minibuffer with completion.
/// In interactive mode, delegates to completing-read with buffer name candidates.
pub(crate) fn builtin_read_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-buffer", &args, 1)?;
    expect_max_args("read-buffer", &args, 4)?;
    let _prompt = expect_string(&args[0])?;

    // Interactive mode: use completing-read with buffer names
    if eval.input_rx.is_some() {
        let prompt = args[0];
        let default = normalize_buffer_reader_default(
            eval.buffer_manager(),
            args.get(1).copied().unwrap_or(Value::Nil),
        );
        let require_match = args.get(2).copied().unwrap_or(Value::Nil);
        let predicate = args.get(3).copied().unwrap_or(Value::Nil);

        // Collect buffer names as a list for completing-read's COLLECTION
        let buf_ids = eval.buffer_manager().buffer_list();
        let buffer_names: Vec<Value> = buf_ids
            .iter()
            .filter_map(|id| eval.buffer_manager().get(*id))
            .map(|b| Value::string(&b.name))
            .collect();
        let collection = Value::list(buffer_names);

        return super::reader::builtin_completing_read(
            eval,
            vec![
                prompt,
                collection,
                predicate,
                require_match,
                Value::Nil,
                Value::Nil,
                default,
            ],
        );
    }

    Err(end_of_file_stdin_error())
}

/// `(read-command PROMPT &optional DEFAULT)`
///
/// Read a command name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer and interns the result.
pub(crate) fn builtin_read_command(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-command", &args, 1)?;
    expect_max_args("read-command", &args, 2)?;
    let _prompt = expect_string(&args[0])?;

    // Interactive mode: read a string and intern it as a symbol
    if eval.input_rx.is_some() {
        let prompt = args[0];
        let default = normalize_symbol_reader_default(args.get(1).copied().unwrap_or(Value::Nil));

        let result = super::reader::builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                default,
            ],
        )?;
        // read-command returns a symbol
        if let Value::Str(id) = result {
            let name = super::value::with_heap(|h| h.get_string(id).to_owned());
            return Ok(Value::symbol(&name));
        }
        return Ok(result);
    }

    Err(end_of_file_stdin_error())
}

/// `(read-variable PROMPT &optional DEFAULT)`
///
/// Read a variable name from the minibuffer.
/// In interactive mode, uses read-from-minibuffer and interns the result.
pub(crate) fn builtin_read_variable(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-variable", &args, 1)?;
    expect_max_args("read-variable", &args, 2)?;
    let _prompt = expect_string(&args[0])?;

    // Interactive mode: read a string and intern it as a symbol
    if eval.input_rx.is_some() {
        let prompt = args[0];
        let default = normalize_symbol_reader_default(args.get(1).copied().unwrap_or(Value::Nil));

        let result = super::reader::builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
                default,
            ],
        )?;
        // read-variable returns a symbol
        if let Value::Str(id) = result {
            let name = super::value::with_heap(|h| h.get_string(id).to_owned());
            return Ok(Value::symbol(&name));
        }
        return Ok(result);
    }

    Err(end_of_file_stdin_error())
}

/// `(try-completion STRING COLLECTION &optional PREDICATE)`
///
/// Returns:
/// - `t` if STRING is an exact and unique match
/// - a string (the longest common prefix) if there are matches
/// - `nil` if no matches
pub(crate) fn builtin_try_completion(args: Vec<Value>) -> EvalResult {
    expect_min_args("try-completion", &args, 2)?;
    expect_max_args("try-completion", &args, 3)?;
    let string = expect_string(&args[0])?;
    let candidates = value_to_string_list(&args[1]);

    let matches: Vec<String> = candidates
        .iter()
        .filter(|c| c.starts_with(&string))
        .cloned()
        .collect();

    if matches.is_empty() {
        return Ok(Value::Nil);
    }

    // Exact unique match?
    if matches.len() == 1 && matches[0] == string {
        return Ok(Value::True);
    }

    // Compute longest common prefix.
    match compute_common_prefix(&matches) {
        Some(prefix) => Ok(Value::string(prefix)),
        None => Ok(Value::Nil),
    }
}

/// `(all-completions STRING COLLECTION &optional PREDICATE)`
///
/// Returns a list of all completions of STRING in COLLECTION.
pub(crate) fn builtin_all_completions(args: Vec<Value>) -> EvalResult {
    expect_min_args("all-completions", &args, 2)?;
    expect_max_args("all-completions", &args, 4)?;
    let string = expect_string(&args[0])?;
    let candidates = value_to_string_list(&args[1]);

    let matches: Vec<Value> = candidates
        .iter()
        .filter(|c| c.starts_with(&string))
        .map(|c| Value::string(c.clone()))
        .collect();

    Ok(Value::list(matches))
}

/// `(test-completion STRING COLLECTION &optional PREDICATE)`
///
/// Returns t if STRING is an exact match in COLLECTION, nil otherwise.
pub(crate) fn builtin_test_completion(args: Vec<Value>) -> EvalResult {
    expect_min_args("test-completion", &args, 2)?;
    expect_max_args("test-completion", &args, 3)?;
    let string = expect_string(&args[0])?;
    let candidates = value_to_string_list(&args[1]);
    Ok(Value::bool(candidates.iter().any(|c| c == &string)))
}

/// `(minibuffer-prompt)` — returns the current minibuffer prompt or nil.
///
/// Stub: returns nil (no active minibuffer in non-interactive mode).
pub(crate) fn builtin_minibuffer_prompt(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-prompt", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_minibuffer_prompt_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_minibuffer_prompt_in_state(&eval.minibuffers, args)
}

pub(crate) fn builtin_minibuffer_prompt_in_state(
    minibuffers: &MinibufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-prompt", &args, 0)?;
    Ok(minibuffers
        .current()
        .map(|state| Value::string(&state.prompt))
        .unwrap_or(Value::Nil))
}

/// `(minibuffer-contents)` — returns the current minibuffer contents.
///
/// In non-interactive batch mode, Emacs exposes current buffer contents.
pub(crate) fn builtin_minibuffer_contents(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_minibuffer_contents_in_state(&eval.minibuffers, &eval.buffers, args)
}

pub(crate) fn builtin_minibuffer_contents_in_state(
    minibuffers: &MinibufferManager,
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-contents", &args, 0)?;
    let text = minibuffer_contents_string(minibuffers, buffers);
    Ok(Value::string(text))
}

/// `(minibuffer-contents-no-properties)` — returns minibuffer contents
/// without text properties.
///
/// NeoVM stores plain strings for this path, so this is equivalent to
/// `minibuffer-contents` in batch mode.
pub(crate) fn builtin_minibuffer_contents_no_properties(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_minibuffer_contents_no_properties_in_state(&eval.minibuffers, &eval.buffers, args)
}

pub(crate) fn builtin_minibuffer_contents_no_properties_in_state(
    minibuffers: &MinibufferManager,
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-contents-no-properties", &args, 0)?;
    let text = minibuffer_contents_string(minibuffers, buffers);
    Ok(Value::string(text))
}

/// `(minibuffer-depth)` — returns the current recursive minibuffer depth.
///
/// Stub: returns 0.
pub(crate) fn builtin_minibuffer_depth(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-depth", &args, 0)?;
    Ok(Value::Int(0))
}

pub(crate) fn builtin_minibuffer_depth_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_minibuffer_depth_in_state(&eval.minibuffers, args)
}

pub(crate) fn builtin_minibuffer_depth_in_state(
    minibuffers: &MinibufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("minibuffer-depth", &args, 0)?;
    Ok(Value::Int(minibuffers.depth() as i64))
}

/// `(minibufferp &optional BUFFER)` — returns t if BUFFER is a minibuffer.
///
/// Batch-compatible behavior: accepts 0..=2 args, validates BUFFER-like first
/// arg shape, and returns nil (no active minibuffer).
pub(crate) fn builtin_minibufferp(args: Vec<Value>) -> EvalResult {
    validate_minibufferp_args(&args)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_minibufferp_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_minibufferp_in_state(&eval.minibuffers, &eval.buffers, args)
}

pub(crate) fn builtin_minibufferp_in_state(
    minibuffers: &MinibufferManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    validate_minibufferp_args(&args)?;
    let live_only = args.get(1).is_some_and(Value::is_truthy);
    let Some(buffer_id) = resolve_minibuffer_buffer_arg(buffers, args.first())? else {
        return Ok(Value::Nil);
    };
    let is_live = minibuffers.has_buffer(buffer_id);
    let is_minibuffer = is_live
        || buffers
            .get(buffer_id)
            .is_some_and(|buffer| is_minibuffer_buffer_name(&buffer.name));
    Ok(Value::bool(if live_only { is_live } else { is_minibuffer }))
}

fn validate_minibufferp_args(args: &[Value]) -> Result<(), Flow> {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("minibufferp"), Value::Int(args.len() as i64)],
        ));
    }
    if let Some(bufferish) = args.first() {
        match bufferish {
            Value::Nil | Value::Str(_) | Value::Buffer(_) => {}
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

/// `(recursive-edit)` — enter a recursive edit.
///
/// Mirrors GNU Emacs keyboard.c:772 `Frecursive_edit`.
/// In interactive mode, enters the command loop (read → execute → redisplay).
/// In batch mode, returns nil.
pub(crate) fn builtin_recursive_edit(args: Vec<Value>) -> EvalResult {
    expect_args("recursive-edit", &args, 0)?;
    // The actual implementation is in Evaluator::recursive_edit() which needs
    // &mut self access.  The builtin dispatch calls this stub for the
    // non-evaluator path; the eval-aware path is registered separately.
    Ok(Value::Nil)
}

/// Eval-aware `(recursive-edit)` — enters the command loop.
///
/// Mirrors GNU Emacs keyboard.c:772 `Frecursive_edit`.
pub(crate) fn builtin_recursive_edit_eval(
    eval: &mut super::eval::Evaluator,
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
        value: Value::Nil,
    })
}

/// `(exit-recursive-edit)` — exit innermost recursive edit.
///
/// Mirrors GNU Emacs keyboard.c:1211 `Fexit_recursive_edit`.
/// Throws to the `exit` tag with nil value.
pub(crate) fn builtin_exit_recursive_edit(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("exit-recursive-edit", &args, 0)?;
    if eval.command_loop.recursive_depth == 0 {
        return Err(signal(
            "user-error",
            vec![Value::string("No recursive edit is in progress")],
        ));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::Nil,
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
        value: Value::Nil,
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

pub(crate) fn builtin_abort_minibuffers_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_abort_minibuffers_in_state(&eval.minibuffers, &eval.buffers, args)
}

pub(crate) fn builtin_abort_minibuffers_in_state(
    minibuffers: &MinibufferManager,
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("abort-minibuffers", &args, 0)?;
    let in_active_minibuffer = buffers
        .current_buffer_id()
        .is_some_and(|buffer_id| minibuffers.has_buffer(buffer_id));
    if !in_active_minibuffer {
        return Err(signal("error", vec![Value::string("Not in a minibuffer")]));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::True,
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
    match bufferish {
        None | Some(Value::Nil) => Ok(buffers.current_buffer_id()),
        Some(Value::Buffer(id)) => Ok(Some(*id)),
        Some(Value::Str(_)) => Ok(bufferish
            .and_then(Value::as_str)
            .and_then(|name| buffers.find_buffer_by_name(name))),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), *other],
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("abort-recursive-edit", &args, 0)?;
    if eval.command_loop.recursive_depth == 0 {
        return Err(signal(
            "user-error",
            vec![Value::string("No recursive edit is in progress")],
        ));
    }
    Err(Flow::Throw {
        tag: Value::symbol("exit"),
        value: Value::True,
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
    match val {
        Value::Nil => Vec::new(),
        Value::Cons(_) => {
            let items = match super::value::list_to_vec(val) {
                Some(v) => v,
                None => return Vec::new(),
            };
            items
                .iter()
                .filter_map(|item| match item {
                    Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
                    Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
                    // Alist entry: (STRING . _)
                    Value::Cons(cell) => {
                        let pair = read_cons(*cell);
                        match &pair.car {
                            Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
                            Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect()
        }
        Value::Vector(v) => {
            let vec = with_heap(|h| h.get_vector(*v).clone());
            vec.iter()
                .filter_map(|item| match item {
                    Value::Str(id) => Some(with_heap(|h| h.get_string(*id).to_owned())),
                    Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
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
