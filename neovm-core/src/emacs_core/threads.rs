//! Emacs-compatible threading primitives for the Elisp VM.
//!
//! Emacs threading is cooperative: only one thread runs at a time, yielding at
//! certain well-defined points.  Since our VM is single-threaded, threads are
//! *simulated* — `make-thread` stores the function and runs it immediately.
//! The key goal is API compatibility so that Elisp packages using threads,
//! mutexes, and condition variables continue to work without error.
//!
//! Provided primitives:
//! - Threads: `make-thread`, `thread-join`, `thread-yield`, `thread-name`,
//!   `thread-live-p`, `threadp`, `thread-signal`, `current-thread`,
//!   `all-threads`, `thread-last-error`
//! - Mutexes: `make-mutex`, `mutexp`, `mutex-name`, `mutex-lock`, `mutex-unlock`
//! - Condition variables: `make-condition-variable`, `condition-variable-p`,
//!   `condition-wait`, `condition-notify`
//! - Special form: `with-mutex`

use std::collections::HashMap;

use super::error::{EvalResult, Flow, make_signal_binding_value, signal, signal_with_data};
use super::value::{Value, eq_value, read_cons};
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// Thread state
// ---------------------------------------------------------------------------

/// Status of a simulated thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    /// Thread has been created but not yet run.
    Created,
    /// Thread is currently running (in our model, at most one can be Running).
    Running,
    /// Thread has finished successfully.
    Finished,
    /// Thread was terminated by an error / signal.
    Signaled,
}

/// Per-thread bookkeeping.
#[derive(Clone, Debug)]
pub struct ThreadState {
    /// Unique thread id.  0 is always the main thread.
    pub id: u64,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// The function to invoke (value passed to `make-thread`).
    pub function: Value,
    /// Current status.
    pub status: ThreadStatus,
    /// Return value after the thread finishes (or nil).
    pub result: Value,
    /// Last error that occurred in this thread, if any.
    pub last_error: Option<Value>,
    /// Whether this thread has already been joined at least once.
    pub joined: bool,
}

// ---------------------------------------------------------------------------
// Mutex state
// ---------------------------------------------------------------------------

/// A cooperative mutex.  Since the VM is single-threaded, lock/unlock are
/// effectively no-ops, but we still track ownership for diagnostics and to
/// match the Emacs API.
#[derive(Clone, Debug)]
pub struct MutexState {
    pub id: u64,
    pub name: Option<String>,
    /// Id of the thread that currently holds the lock, or `None`.
    pub owner: Option<u64>,
    /// Recursive lock count.
    pub lock_count: u32,
}

// ---------------------------------------------------------------------------
// Condition variable state
// ---------------------------------------------------------------------------

/// A condition variable bound to a particular mutex.
#[derive(Clone, Debug)]
pub struct ConditionVarState {
    pub id: u64,
    pub name: Option<String>,
    /// The mutex id this condition variable is associated with.
    pub mutex_id: u64,
}

// ---------------------------------------------------------------------------
// ThreadManager
// ---------------------------------------------------------------------------

/// Central registry for threads, mutexes, and condition variables.
pub struct ThreadManager {
    threads: HashMap<u64, ThreadState>,
    next_id: u64,
    current_thread: u64, // ID of currently running thread (0 = main)
    thread_handles: HashMap<u64, Value>,
    mutexes: HashMap<u64, MutexState>,
    next_mutex_id: u64,
    mutex_handles: HashMap<u64, Value>,
    condition_vars: HashMap<u64, ConditionVarState>,
    next_cv_id: u64,
    condition_var_handles: HashMap<u64, Value>,
    /// Global last-error value (returned by `thread-last-error`).
    last_error: Option<Value>,
}

impl ThreadManager {
    /// Create a new manager with the main thread pre-registered.
    pub fn new() -> Self {
        let mut threads = HashMap::new();
        threads.insert(
            0,
            ThreadState {
                id: 0,
                name: None,
                function: Value::Nil,
                status: ThreadStatus::Running,
                result: Value::Nil,
                last_error: None,
                joined: false,
            },
        );
        let mut thread_handles = HashMap::new();
        thread_handles.insert(0, tagged_object_value("thread", 0));
        Self {
            threads,
            next_id: 1,
            current_thread: 0,
            thread_handles,
            mutexes: HashMap::new(),
            next_mutex_id: 1,
            mutex_handles: HashMap::new(),
            condition_vars: HashMap::new(),
            next_cv_id: 1,
            condition_var_handles: HashMap::new(),
            last_error: None,
        }
    }

    // -- Thread operations --------------------------------------------------

    /// Create a new thread.  Returns the id.
    pub fn create_thread(&mut self, function: Value, name: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.threads.insert(
            id,
            ThreadState {
                id,
                name,
                function,
                status: ThreadStatus::Created,
                result: Value::Nil,
                last_error: None,
                joined: false,
            },
        );
        self.thread_handles
            .insert(id, tagged_object_value("thread", id));
        id
    }

    /// Mark a thread as Running.
    pub fn start_thread(&mut self, id: u64) {
        if let Some(t) = self.threads.get_mut(&id) {
            t.status = ThreadStatus::Running;
        }
    }

    /// Set the currently running thread and return the previous thread id.
    pub fn enter_thread(&mut self, id: u64) -> u64 {
        let saved = self.current_thread;
        self.current_thread = id;
        saved
    }

    /// Restore the previously running thread id.
    pub fn restore_thread(&mut self, id: u64) {
        self.current_thread = id;
    }

    /// Mark a thread as Finished with the given result.
    pub fn finish_thread(&mut self, id: u64, result: Value) {
        if let Some(t) = self.threads.get_mut(&id) {
            t.status = ThreadStatus::Finished;
            t.result = result;
        }
    }

    /// Mark a thread as Signaled with an error value.
    pub fn signal_thread(&mut self, id: u64, error: Value) {
        if let Some(t) = self.threads.get_mut(&id) {
            t.status = ThreadStatus::Signaled;
            t.last_error = Some(error);
        }
    }

    /// Publish an error value for `thread-last-error`.
    pub fn record_last_error(&mut self, error: Value) {
        self.last_error = Some(error);
    }

    /// Get the id of the currently running thread.
    pub fn current_thread_id(&self) -> u64 {
        self.current_thread
    }

    /// Look up a thread by id.
    pub fn get_thread(&self, id: u64) -> Option<&ThreadState> {
        self.threads.get(&id)
    }

    /// Check if a thread is alive (Created or Running).
    pub fn thread_alive_p(&self, id: u64) -> bool {
        self.threads
            .get(&id)
            .is_some_and(|t| t.status == ThreadStatus::Created || t.status == ThreadStatus::Running)
    }

    /// Get thread name.
    pub fn thread_name(&self, id: u64) -> Option<&str> {
        self.threads.get(&id).and_then(|t| t.name.as_deref())
    }

    /// Check if a value represents a known thread id.
    pub fn is_thread(&self, id: u64) -> bool {
        self.threads.contains_key(&id)
    }

    /// Return canonical handle object for thread id.
    pub fn thread_handle(&self, id: u64) -> Option<Value> {
        self.thread_handles.get(&id).cloned()
    }

    /// Return the thread id iff VALUE is the canonical thread handle object.
    pub fn thread_id_from_handle(&self, value: &Value) -> Option<u64> {
        canonical_handle_id(&self.thread_handles, value, "thread")
    }

    /// Return all thread ids.
    pub fn all_thread_ids(&self) -> Vec<u64> {
        self.threads
            .iter()
            .filter_map(|(id, thread)| (!thread.joined).then_some(*id))
            .collect()
    }

    /// Return thread result (for join).
    pub fn thread_result(&self, id: u64) -> Value {
        self.threads
            .get(&id)
            .map(|t| t.result)
            .unwrap_or(Value::Nil)
    }

    /// Mark a thread as joined. Returns its terminal error only on first join.
    pub fn join_thread(&mut self, id: u64) -> Option<Value> {
        let thread = self.threads.get_mut(&id)?;
        if thread.joined {
            return None;
        }
        thread.joined = true;
        thread.last_error
    }

    /// Get and optionally clear the global last-error.
    pub fn last_error(&mut self, cleanup: bool) -> Value {
        let val = self.last_error.unwrap_or(Value::Nil);
        if cleanup {
            self.last_error = None;
        }
        val
    }

    // -- Mutex operations ---------------------------------------------------

    /// Create a new mutex.  Returns the id.
    pub fn create_mutex(&mut self, name: Option<String>) -> u64 {
        let id = self.next_mutex_id;
        self.next_mutex_id += 1;
        self.mutexes.insert(
            id,
            MutexState {
                id,
                name,
                owner: None,
                lock_count: 0,
            },
        );
        self.mutex_handles
            .insert(id, tagged_object_value("mutex", id));
        id
    }

    /// Lock a mutex (on behalf of the current thread).
    /// In single-threaded mode this always succeeds.
    pub fn mutex_lock(&mut self, mutex_id: u64) -> bool {
        let current = self.current_thread;
        if let Some(m) = self.mutexes.get_mut(&mutex_id) {
            match m.owner {
                None => {
                    m.owner = Some(current);
                    m.lock_count = 1;
                    true
                }
                Some(owner) if owner == current => {
                    // Recursive lock.
                    m.lock_count += 1;
                    true
                }
                Some(_) => {
                    // In a real multi-threaded implementation this would block.
                    // In our single-threaded sim it should not happen, but we
                    // handle it gracefully by allowing the lock anyway.
                    m.owner = Some(current);
                    m.lock_count = 1;
                    true
                }
            }
        } else {
            false
        }
    }

    /// Unlock a mutex.  Returns false if the mutex doesn't exist or is
    /// not held by the current thread.
    pub fn mutex_unlock(&mut self, mutex_id: u64) -> bool {
        let current = self.current_thread;
        if let Some(m) = self.mutexes.get_mut(&mutex_id) {
            if m.owner == Some(current) {
                m.lock_count = m.lock_count.saturating_sub(1);
                if m.lock_count == 0 {
                    m.owner = None;
                }
                true
            } else {
                // Not locked by current thread — still succeed for compatibility
                true
            }
        } else {
            false
        }
    }

    /// Check if a value represents a known mutex id.
    pub fn is_mutex(&self, id: u64) -> bool {
        self.mutexes.contains_key(&id)
    }

    /// Return canonical handle object for mutex id.
    pub fn mutex_handle(&self, id: u64) -> Option<Value> {
        self.mutex_handles.get(&id).cloned()
    }

    /// Return the mutex id iff VALUE is the canonical mutex handle object.
    pub fn mutex_id_from_handle(&self, value: &Value) -> Option<u64> {
        canonical_handle_id(&self.mutex_handles, value, "mutex")
    }

    /// Get mutex name.
    pub fn mutex_name(&self, id: u64) -> Option<&str> {
        self.mutexes.get(&id).and_then(|m| m.name.as_deref())
    }

    /// Return true when MUTEX-ID is owned by the current thread.
    pub fn mutex_owned_by_current_thread(&self, mutex_id: u64) -> bool {
        self.mutexes
            .get(&mutex_id)
            .is_some_and(|m| m.owner == Some(self.current_thread))
    }

    // -- Condition variable operations --------------------------------------

    /// Create a condition variable associated with the given mutex.
    pub fn create_condition_variable(
        &mut self,
        mutex_id: u64,
        name: Option<String>,
    ) -> Option<u64> {
        if !self.mutexes.contains_key(&mutex_id) {
            return None;
        }
        let id = self.next_cv_id;
        self.next_cv_id += 1;
        self.condition_vars
            .insert(id, ConditionVarState { id, name, mutex_id });
        self.condition_var_handles
            .insert(id, tagged_object_value("condition-variable", id));
        Some(id)
    }

    /// Check if a value represents a known condition variable id.
    pub fn is_condition_variable(&self, id: u64) -> bool {
        self.condition_vars.contains_key(&id)
    }

    /// Return canonical handle object for condition variable id.
    pub fn condition_variable_handle(&self, id: u64) -> Option<Value> {
        self.condition_var_handles.get(&id).cloned()
    }

    /// Return the condition variable id iff VALUE is the canonical handle.
    pub fn condition_variable_id_from_handle(&self, value: &Value) -> Option<u64> {
        canonical_handle_id(&self.condition_var_handles, value, "condition-variable")
    }

    /// Get condition variable name.
    pub fn condition_variable_name(&self, id: u64) -> Option<&str> {
        self.condition_vars
            .get(&id)
            .and_then(|cv| cv.name.as_deref())
    }

    /// Get the mutex associated with a condition variable.
    pub fn condition_variable_mutex(&self, cv_id: u64) -> Option<u64> {
        self.condition_vars.get(&cv_id).map(|cv| cv.mutex_id)
    }
}

impl Default for ThreadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for ThreadManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for thread in self.threads.values() {
            roots.push(thread.function);
            roots.push(thread.result);
            if let Some(ref err) = thread.last_error {
                roots.push(*err);
            }
        }
        for value in self.thread_handles.values() {
            roots.push(*value);
        }
        for value in self.mutex_handles.values() {
            roots.push(*value);
        }
        for value in self.condition_var_handles.values() {
            roots.push(*value);
        }
        if let Some(ref err) = self.last_error {
            roots.push(*err);
        }
    }
}

// ===========================================================================
// Argument helpers
// ===========================================================================

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

fn expect_args_range(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn tagged_object_value(tag: &str, id: u64) -> Value {
    Value::cons(Value::symbol(tag), Value::Int(id as i64))
}

fn tagged_object_id(value: &Value, expected_tag: &str) -> Option<u64> {
    let Value::Cons(cell) = value else {
        return None;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some(expected_tag) {
        return None;
    }
    match pair.cdr {
        Value::Int(n) if n >= 0 => Some(n as u64),
        _ => None,
    }
}

fn canonical_handle_id(handles: &HashMap<u64, Value>, value: &Value, tag: &str) -> Option<u64> {
    let id = tagged_object_id(value, tag)?;
    let canonical = handles.get(&id)?;
    if eq_value(canonical, value) {
        Some(id)
    } else {
        None
    }
}

/// Extract a thread id from a canonical thread handle object.
fn expect_thread_id(manager: &ThreadManager, value: &Value) -> Result<u64, Flow> {
    match manager.thread_id_from_handle(value) {
        Some(id) => Ok(id),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), *value],
        )),
    }
}

/// Extract a mutex id from a canonical mutex handle object.
fn expect_mutex_id(manager: &ThreadManager, value: &Value) -> Result<u64, Flow> {
    match manager.mutex_id_from_handle(value) {
        Some(id) => Ok(id),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), *value],
        )),
    }
}

/// Extract a condition variable id from a canonical handle object.
fn expect_cv_id(manager: &ThreadManager, value: &Value) -> Result<u64, Flow> {
    match manager.condition_variable_id_from_handle(value) {
        Some(id) => Ok(id),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), *value],
        )),
    }
}

// ===========================================================================
// Thread builtins
// ===========================================================================

/// `(make-thread FUNCTION &optional NAME)` -- create a thread.
///
/// In our single-threaded simulation the function is executed immediately.
/// Returns a `(thread . ID)` object.
pub(crate) fn builtin_make_thread(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let (thread_id, function) = prepare_make_thread_in_state(&mut eval.threads, &args)?;
    let saved_current = eval.threads.enter_thread(thread_id);
    let result = eval.apply(function, vec![]);
    eval.threads.restore_thread(saved_current);
    finish_make_thread_result_in_state(&mut eval.threads, thread_id, result)
}

pub(crate) fn prepare_make_thread_in_state(
    threads: &mut ThreadManager,
    args: &[Value],
) -> Result<(u64, Value), Flow> {
    expect_args_range("make-thread", args, 1, 3)?;

    let function = args[0];
    let name = if args.len() > 1 {
        match &args[1] {
            Value::Str(_) => Some(args[1].as_str().unwrap().to_string()),
            Value::Nil => None,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    } else {
        None
    };

    let thread_id = threads.create_thread(function, name);
    threads.start_thread(thread_id);
    Ok((thread_id, function))
}

pub(crate) fn finish_make_thread_in_eval(
    eval: &mut super::eval::Evaluator,
    thread_id: u64,
    function: Value,
) -> EvalResult {
    let saved_current = eval.threads.enter_thread(thread_id);
    let result = eval.apply(function, vec![]);
    eval.threads.restore_thread(saved_current);
    finish_make_thread_result_in_state(&mut eval.threads, thread_id, result)
}

pub(crate) fn finish_make_thread_result_in_state(
    threads: &mut ThreadManager,
    thread_id: u64,
    result: EvalResult,
) -> EvalResult {
    match result {
        Ok(val) => {
            threads.finish_thread(thread_id, val);
        }
        Err(Flow::Signal(ref sig)) => {
            let error_val = make_signal_binding_value(sig);
            threads.signal_thread(thread_id, error_val);
            // Don't record_last_error here — in GNU, thread errors are
            // published asynchronously when the thread exits.  Since neomacs
            // runs threads synchronously, defer to thread-join to match
            // the expected timing (thread-last-error is nil before join).
        }
        Err(Flow::Throw { ref tag, ref value }) => {
            let error_val = Value::list(vec![Value::symbol("no-catch"), *tag, *value]);
            threads.signal_thread(thread_id, error_val);
        }
    }

    Ok(threads
        .thread_handle(thread_id)
        .unwrap_or_else(|| tagged_object_value("thread", thread_id)))
}

/// `(thread-join THREAD)` -- wait for thread completion.
///
/// Since all threads run synchronously at creation time, they are already
/// finished by the time anyone can call join.  Returns the thread's result.
pub(crate) fn builtin_thread_join_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("thread-join", &args, 1)?;
    let id = expect_thread_id(threads, &args[0])?;
    if !threads.is_thread(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), args[0]],
        ));
    }
    if id == threads.current_thread_id() {
        return Err(signal(
            "error",
            vec![Value::string("Cannot join current thread")],
        ));
    }
    if let Some(error) = threads.join_thread(id) {
        // Emacs publishes joined-thread terminal errors through thread-last-error.
        threads.record_last_error(error);
    }
    Ok(threads.thread_result(id))
}

pub(crate) fn builtin_thread_join(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_join_in_state(&mut eval.threads, args)
}

/// `(thread-yield)` -- yield the current thread.
///
/// No-op in our single-threaded simulation.
pub(crate) fn builtin_thread_yield_in_state(args: Vec<Value>) -> EvalResult {
    expect_args("thread-yield", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_thread_yield(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_yield_in_state(args)
}

/// `(thread-name THREAD)` -- return the thread's name or nil.
pub(crate) fn builtin_thread_name_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("thread-name", &args, 1)?;
    let id = expect_thread_id(threads, &args[0])?;
    if !threads.is_thread(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), args[0]],
        ));
    }
    match threads.thread_name(id) {
        Some(name) => Ok(Value::string(name)),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_thread_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_name_in_state(&eval.threads, args)
}

/// `(thread-live-p THREAD)` -- check if the thread is alive.
pub(crate) fn builtin_thread_live_p_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("thread-live-p", &args, 1)?;
    let id = expect_thread_id(threads, &args[0])?;
    if !threads.is_thread(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), args[0]],
        ));
    }
    Ok(Value::bool(threads.thread_alive_p(id)))
}

pub(crate) fn builtin_thread_live_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_live_p_in_state(&eval.threads, args)
}

/// `(threadp OBJ)` -- type predicate.
///
/// Returns t if OBJ is a known thread object.
pub(crate) fn builtin_threadp_in_state(threads: &ThreadManager, args: Vec<Value>) -> EvalResult {
    expect_args("threadp", &args, 1)?;
    Ok(Value::bool(
        threads.thread_id_from_handle(&args[0]).is_some(),
    ))
}

pub(crate) fn builtin_threadp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_threadp_in_state(&eval.threads, args)
}

/// `(thread-signal THREAD ERROR-SYMBOL DATA)` -- send a signal to a thread.
///
/// In our simulation this simply records the error on the target thread.
pub(crate) fn builtin_thread_signal_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("thread-signal", &args, 3)?;
    let id = expect_thread_id(threads, &args[0])?;
    if !threads.is_thread(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), args[0]],
        ));
    }
    let error_symbol = args[1];
    let Some(error_name) = error_symbol.as_symbol_name() else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), error_symbol],
        ));
    };
    let data = args[2];
    if id == threads.current_thread_id() {
        return Err(signal_with_data(error_name, data));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_thread_signal(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_signal_in_state(&eval.threads, args)
}

/// `(current-thread)` -- return the current thread object.
pub(crate) fn builtin_current_thread_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-thread", &args, 0)?;
    let id = threads.current_thread_id();
    Ok(threads
        .thread_handle(id)
        .unwrap_or_else(|| tagged_object_value("thread", id)))
}

pub(crate) fn builtin_current_thread(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_current_thread_in_state(&eval.threads, args)
}

/// `(all-threads)` -- return a list of all thread objects.
pub(crate) fn builtin_all_threads_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("all-threads", &args, 0)?;
    let mut ids = threads.all_thread_ids();
    ids.sort_unstable();
    let objects: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            threads
                .thread_handle(id)
                .unwrap_or_else(|| tagged_object_value("thread", id))
        })
        .collect();
    Ok(Value::list(objects))
}

pub(crate) fn builtin_all_threads(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_all_threads_in_state(&eval.threads, args)
}

/// `(thread-last-error &optional CLEANUP)` -- return the last error.
///
/// If CLEANUP is non-nil, clear the stored error after returning it.
pub(crate) fn builtin_thread_last_error_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("thread-last-error"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let cleanup = args.first().is_some_and(|v| v.is_truthy());
    Ok(threads.last_error(cleanup))
}

pub(crate) fn builtin_thread_last_error(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_thread_last_error_in_state(&mut eval.threads, args)
}

// ===========================================================================
// Mutex builtins
// ===========================================================================

/// `(make-mutex &optional NAME)` -- create a mutex.
pub(crate) fn builtin_make_mutex_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("make-mutex"), Value::Int(args.len() as i64)],
        ));
    }
    let name = if let Some(v) = args.first() {
        match v {
            Value::Str(_) => Some(v.as_str().unwrap().to_string()),
            Value::Nil => None,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    } else {
        None
    };
    let id = threads.create_mutex(name);
    Ok(threads
        .mutex_handle(id)
        .unwrap_or_else(|| tagged_object_value("mutex", id)))
}

pub(crate) fn builtin_make_mutex(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_mutex_in_state(&mut eval.threads, args)
}

/// `(mutexp OBJ)` -- type predicate for mutexes.
pub(crate) fn builtin_mutexp_in_state(threads: &ThreadManager, args: Vec<Value>) -> EvalResult {
    expect_args("mutexp", &args, 1)?;
    Ok(Value::bool(
        threads.mutex_id_from_handle(&args[0]).is_some(),
    ))
}

pub(crate) fn builtin_mutexp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_mutexp_in_state(&eval.threads, args)
}

/// `(mutex-name MUTEX)` -- return the mutex's name or nil.
pub(crate) fn builtin_mutex_name_in_state(threads: &ThreadManager, args: Vec<Value>) -> EvalResult {
    expect_args("mutex-name", &args, 1)?;
    let id = expect_mutex_id(threads, &args[0])?;
    if !threads.is_mutex(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), args[0]],
        ));
    }
    match threads.mutex_name(id) {
        Some(name) => Ok(Value::string(name)),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_mutex_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_mutex_name_in_state(&eval.threads, args)
}

/// `(mutex-lock MUTEX)` -- lock a mutex.
///
/// In single-threaded mode, this always succeeds immediately.
pub(crate) fn builtin_mutex_lock_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mutex-lock", &args, 1)?;
    let id = expect_mutex_id(threads, &args[0])?;
    if !threads.is_mutex(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), args[0]],
        ));
    }
    threads.mutex_lock(id);
    Ok(Value::Nil)
}

pub(crate) fn builtin_mutex_lock(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_mutex_lock_in_state(&mut eval.threads, args)
}

/// `(mutex-unlock MUTEX)` -- unlock a mutex.
pub(crate) fn builtin_mutex_unlock_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mutex-unlock", &args, 1)?;
    let id = expect_mutex_id(threads, &args[0])?;
    if !threads.is_mutex(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), args[0]],
        ));
    }
    threads.mutex_unlock(id);
    Ok(Value::Nil)
}

pub(crate) fn builtin_mutex_unlock(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_mutex_unlock_in_state(&mut eval.threads, args)
}

// ===========================================================================
// Condition variable builtins
// ===========================================================================

/// `(make-condition-variable MUTEX &optional NAME)` -- create a condition variable.
pub(crate) fn builtin_make_condition_variable_in_state(
    threads: &mut ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("make-condition-variable", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-condition-variable"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let mutex_id = expect_mutex_id(threads, &args[0])?;
    if !threads.is_mutex(mutex_id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), args[0]],
        ));
    }
    let name = if args.len() > 1 {
        match &args[1] {
            Value::Str(_) => Some(args[1].as_str().unwrap().to_string()),
            Value::Nil => None,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    } else {
        None
    };
    match threads.create_condition_variable(mutex_id, name) {
        Some(id) => Ok(threads
            .condition_variable_handle(id)
            .unwrap_or_else(|| tagged_object_value("condition-variable", id))),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_make_condition_variable(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_condition_variable_in_state(&mut eval.threads, args)
}

/// `(condition-variable-p OBJ)` -- type predicate.
pub(crate) fn builtin_condition_variable_p_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("condition-variable-p", &args, 1)?;
    Ok(Value::bool(
        threads
            .condition_variable_id_from_handle(&args[0])
            .is_some(),
    ))
}

pub(crate) fn builtin_condition_variable_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_condition_variable_p_in_state(&eval.threads, args)
}

/// `(condition-name COND)` -- return COND's name string, or nil when unnamed.
pub(crate) fn builtin_condition_name_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("condition-name", &args, 1)?;
    let id = expect_cv_id(threads, &args[0])?;
    if !threads.is_condition_variable(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    }
    match threads.condition_variable_name(id) {
        Some(name) => Ok(Value::string(name)),
        None => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_condition_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_condition_name_in_state(&eval.threads, args)
}

/// `(condition-mutex COND)` -- return COND's associated mutex object.
pub(crate) fn builtin_condition_mutex_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("condition-mutex", &args, 1)?;
    let id = expect_cv_id(threads, &args[0])?;
    if !threads.is_condition_variable(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    }
    let Some(mutex_id) = threads.condition_variable_mutex(id) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    };
    threads.mutex_handle(mutex_id).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        )
    })
}

pub(crate) fn builtin_condition_mutex(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_condition_mutex_in_state(&eval.threads, args)
}

/// `(condition-wait COND)` -- wait on a condition variable.
///
/// In single-threaded mode this is a no-op.
pub(crate) fn builtin_condition_wait_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("condition-wait", &args, 1)?;
    let id = expect_cv_id(threads, &args[0])?;
    if !threads.is_condition_variable(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    }
    let Some(mutex_id) = threads.condition_variable_mutex(id) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    };
    if !threads.mutex_owned_by_current_thread(mutex_id) {
        return Err(signal(
            "error",
            vec![Value::string(
                "Condition variable’s mutex is not held by current thread",
            )],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_condition_wait(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_condition_wait_in_state(&eval.threads, args)
}

/// `(condition-notify COND &optional ALL)` -- notify on a condition variable.
///
/// No-op in single-threaded mode.
pub(crate) fn builtin_condition_notify_in_state(
    threads: &ThreadManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("condition-notify", &args, 1)?;
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("condition-notify"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let id = expect_cv_id(threads, &args[0])?;
    if !threads.is_condition_variable(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    }
    let Some(mutex_id) = threads.condition_variable_mutex(id) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("condition-variable-p"), args[0]],
        ));
    };
    if !threads.mutex_owned_by_current_thread(mutex_id) {
        return Err(signal(
            "error",
            vec![Value::string(
                "Condition variable’s mutex is not held by current thread",
            )],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_condition_notify(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_condition_notify_in_state(&eval.threads, args)
}

// ===========================================================================
// Special form: with-mutex
// ===========================================================================

/// `(with-mutex MUTEX BODY...)` -- execute BODY with MUTEX locked.
///
/// This is a special form: MUTEX is evaluated, the lock is acquired, BODY is
/// executed as an implicit progn, and the lock is released on exit
/// (even if BODY signals an error).
pub(crate) fn sf_with_mutex(
    eval: &mut super::eval::Evaluator,
    tail: &[super::expr::Expr],
) -> EvalResult {
    if tail.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
        ));
    }
    let mutex_val = eval.eval(&tail[0])?;
    let mutex_id = expect_mutex_id(&eval.threads, &mutex_val)?;
    if !eval.threads.is_mutex(mutex_id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("mutexp"), mutex_val],
        ));
    }

    eval.threads.mutex_lock(mutex_id);
    let result = eval.sf_progn(&tail[1..]);
    eval.threads.mutex_unlock(mutex_id);
    result
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "threads_test.rs"]
mod tests;
