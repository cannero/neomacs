//! Context — special forms, function application, and dispatch.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// GC-rooted constant pool for Values embedded in Expr trees.
/// Replaces Expr::OpaqueValue(Value) with Expr::OpaqueValueRef(u32).
/// Values in this pool are always traced by GC — no stale references.
pub(crate) struct OpaqueValuePool {
    values: Vec<Option<Value>>,
    free_list: Vec<u32>,
}

impl OpaqueValuePool {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            free_list: Vec::new(),
        }
    }

    pub fn insert(&mut self, val: Value) -> u32 {
        if let Some(idx) = self.free_list.pop() {
            self.values[idx as usize] = Some(val);
            idx
        } else {
            let idx = self.values.len() as u32;
            self.values.push(Some(val));
            idx
        }
    }

    pub fn get(&self, idx: u32) -> Value {
        self.values[idx as usize].unwrap_or(Value::NIL)
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, idx: u32) {
        self.values[idx as usize] = None;
        self.free_list.push(idx);
    }

    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        for val in &self.values {
            if let Some(v) = val {
                roots.push(*v);
            }
        }
    }
}

thread_local! {
    /// Thread-local opaque value pool used by `value_to_expr` and other code
    /// that creates `Expr::OpaqueValueRef` nodes without access to `Context`.
    pub(crate) static OPAQUE_POOL: RefCell<OpaqueValuePool> = RefCell::new(OpaqueValuePool::new());
}

/// Insert a Value into the thread-local OpaqueValuePool and return its index.
/// Use the returned index with `Expr::OpaqueValueRef(idx)`.
pub fn opaque_pool_insert(val: Value) -> u32 {
    OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(val))
}

use super::abbrev::AbbrevManager;
use super::advice::VariableWatcherList;
use super::autoload::AutoloadManager;
use super::bookmark::BookmarkManager;
use super::builtins;
use super::bytecode::Compiler;
use super::coding::CodingSystemManager;
use super::custom::CustomManager;
use super::doc::{STARTUP_VARIABLE_DOC_STRING_PROPERTIES, STARTUP_VARIABLE_DOC_STUBS};
use super::error::*;
use super::expr::Expr;
use super::interactive::InteractiveRegistry;
use super::intern::{
    StringInterner, SymId, clear_current_interner, current_interner_ptr, intern, intern_uninterned,
    lookup_interned, resolve_sym, set_current_interner,
};
use super::keymap::{
    list_keymap_define, list_keymap_set_parent, make_list_keymap, make_sparse_list_keymap,
};
use super::kmacro::KmacroManager;
use super::minibuffer::MinibufferManager;
use super::mode::ModeRegistry;
use super::process::ProcessManager;
use super::rect::RectangleState;
use super::regex::MatchData;
use super::register::RegisterManager;
use super::symbol::Obarray;
use super::threads::ThreadManager;
use super::timer::TimerManager;
use super::value::*;
use crate::buffer::{BufferManager, InsertionType};
use crate::face::{Face as RuntimeFace, FaceTable, FontSlant, FontWeight, FontWidth};
use crate::gc::GcTrace;
use crate::window::FrameManager;

const EVAL_STACK_RED_ZONE: usize = 256 * 1024;
const EVAL_STACK_SEGMENT: usize = 32 * 1024 * 1024;
const NAMED_CALL_CACHE_CAPACITY: usize = 8;

#[derive(Clone, Debug)]
struct ExecutingKbdMacroRuntimeScope {
    snapshot: crate::keyboard::ExecutingKbdMacroRuntimeSnapshot,
    real_this_command: Value,
}

/// A single entry on the specpdl (special binding stack).
/// Matches GNU Emacs's `union specbinding` SPECPDL_LET / SPECPDL_LET_LOCAL.
#[derive(Clone, Debug)]
pub(crate) enum SpecBinding {
    /// Plain dynamic let-binding: saves old obarray (global/default) value.
    Let {
        sym_id: SymId,
        old_value: Option<Value>,
    },
    /// Buffer-local let-binding: saves old buffer-local value and which buffer.
    /// On unbind, restores the value in that specific buffer (if still live).
    /// Matches GNU's SPECPDL_LET_LOCAL.
    LetLocal {
        sym_id: SymId,
        old_value: Value,
        buffer_id: crate::buffer::BufferId,
    },
    /// Default-value let-binding for buffer-local variables without a local
    /// binding in the current buffer. Saves/restores the obarray default value.
    /// Matches GNU's SPECPDL_LET_DEFAULT.
    LetDefault {
        sym_id: SymId,
        old_value: Option<Value>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeBacktraceFrame {
    pub(crate) function: Value,
    pub(crate) args: Vec<Value>,
    pub(crate) evaluated: bool,
    pub(crate) debug_on_exit: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingSafeFuncall {
    pub(crate) function: Value,
    pub(crate) args: Vec<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GnuTimerTimestamp {
    pub(crate) high_seconds: i64,
    pub(crate) low_seconds: i64,
    pub(crate) usecs: i64,
    pub(crate) psecs: i64,
}

impl GnuTimerTimestamp {
    pub(crate) fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let (secs, usecs) = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(dur) => (dur.as_secs() as i64, dur.subsec_micros() as i64),
            Err(err) => {
                let dur = err.duration();
                (-(dur.as_secs() as i64), -(dur.subsec_micros() as i64))
            }
        };

        Self {
            high_seconds: secs >> 16,
            low_seconds: secs & 0xFFFF,
            usecs,
            psecs: 0,
        }
    }

    fn unix_seconds(self) -> i64 {
        (self.high_seconds << 16) + self.low_seconds
    }

    pub(crate) fn duration_until(self, now: Self) -> std::time::Duration {
        use std::time::Duration;

        if self <= now {
            return Duration::ZERO;
        }

        let mut secs = self.unix_seconds() - now.unix_seconds();
        let mut usecs = self.usecs - now.usecs;
        let mut psecs = self.psecs - now.psecs;

        if psecs < 0 {
            psecs += 1_000_000;
            usecs -= 1;
        }
        if usecs < 0 {
            usecs += 1_000_000;
            secs -= 1;
        }
        if secs < 0 {
            return Duration::ZERO;
        }

        let mut secs = secs as u64;
        let mut nanos = (usecs as u32) * 1_000 + ((psecs.max(0) as u32) + 999) / 1_000;
        if nanos >= 1_000_000_000 {
            secs += 1;
            nanos -= 1_000_000_000;
        }

        Duration::new(secs, nanos)
    }

    pub(crate) fn overdue_duration(self, now: Self) -> std::time::Duration {
        use std::time::Duration;

        if self >= now {
            return Duration::ZERO;
        }

        let mut secs = now.unix_seconds() - self.unix_seconds();
        let mut usecs = now.usecs - self.usecs;
        let mut psecs = now.psecs - self.psecs;

        if psecs < 0 {
            psecs += 1_000_000;
            usecs -= 1;
        }
        if usecs < 0 {
            usecs += 1_000_000;
            secs -= 1;
        }

        let nanos = ((usecs as u32) * 1_000) + (psecs as u32 / 1_000);
        Duration::new(secs as u64, nanos)
    }

    pub(crate) fn from_duration(duration: std::time::Duration) -> Self {
        let secs = duration.as_secs() as i64;
        let usecs = duration.subsec_micros() as i64;
        Self {
            high_seconds: secs >> 16,
            low_seconds: secs & 0xFFFF,
            usecs,
            psecs: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingGnuTimer {
    pub(crate) timer: Value,
    pub(crate) when: GnuTimerTimestamp,
}

/// Compute a content fingerprint of a macro call's args slice.
///
/// Used to detect ABA in the macro expansion cache: when a lambda body
/// `Rc<Vec<Expr>>` is freed and its memory reused, `tail.as_ptr()` can
/// match a stale cache entry.  The fingerprint catches this by hashing
/// a summary of the actual Expr nodes.
fn tail_fingerprint(tail: &[Expr]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tail.len().hash(&mut hasher);
    for (i, expr) in tail.iter().enumerate() {
        i.hash(&mut hasher);
        expr_fingerprint(expr, &mut hasher, 3);
    }
    hasher.finish()
}

fn expr_fingerprint(expr: &Expr, hasher: &mut impl std::hash::Hasher, depth: usize) {
    use std::hash::Hash;
    std::mem::discriminant(expr).hash(hasher);
    if depth == 0 {
        return;
    }
    match expr {
        Expr::Symbol(id) | Expr::Keyword(id) => id.0.hash(hasher),
        Expr::Int(n) => n.hash(hasher),
        Expr::Char(c) => c.hash(hasher),
        Expr::Float(f) => f.to_bits().hash(hasher),
        Expr::Str(s) => s.hash(hasher),
        Expr::ReaderLoadFileName => {}
        Expr::Bool(b) => b.hash(hasher),
        Expr::List(items) | Expr::Vector(items) => {
            items.len().hash(hasher);
            for item in items.iter().take(4) {
                expr_fingerprint(item, hasher, depth - 1);
            }
        }
        Expr::DottedList(items, tail) => {
            items.len().hash(hasher);
            for item in items.iter().take(3) {
                expr_fingerprint(item, hasher, depth - 1);
            }
            expr_fingerprint(tail, hasher, depth - 1);
        }
        Expr::OpaqueValueRef(idx) => {
            idx.hash(hasher);
        }
    }
}

fn interpreted_closure_env_entries(lexenv: Value) -> Vec<InterpretedClosureEnvEntry> {
    let mut cursor = lexenv;
    let mut entries = Vec::new();
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                match pair_car.kind() {
                    ValueKind::T => entries.push(InterpretedClosureEnvEntry::TopLevelSentinel),
                    ValueKind::Symbol(sym) => {
                        entries.push(InterpretedClosureEnvEntry::Special(sym))
                    }
                    ValueKind::Cons => {
                        let inner_car = pair_car.cons_car();
                        if let Some(sym) = binding_symbol_id(inner_car) {
                            entries.push(InterpretedClosureEnvEntry::Binding(sym));
                        }
                    }
                    _ => {}
                }
                cursor = pair_cdr;
            }
            _ => return entries,
        }
    }
}

fn binding_symbol_id(value: Value) -> Option<SymId> {
    match value.kind() {
        ValueKind::Symbol(sym) => Some(sym),
        ValueKind::T => Some(intern("t")),
        ValueKind::Nil => Some(intern("nil")),
        _ => None,
    }
}

fn interpreted_closure_trim_fingerprint(
    params_expr: &Expr,
    body_exprs: &[Expr],
    iform_expr: &Expr,
    env_shape: &[InterpretedClosureEnvEntry],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    expr_fingerprint(params_expr, &mut hasher, 8);
    body_exprs.len().hash(&mut hasher);
    for expr in body_exprs {
        expr_fingerprint(expr, &mut hasher, 8);
    }
    expr_fingerprint(iform_expr, &mut hasher, 8);
    env_shape.hash(&mut hasher);
    hasher.finish()
}

fn rebuild_trimmed_interpreted_closure_env(
    source_env: Value,
    template: &[InterpretedClosureEnvEntry],
) -> Value {
    let mut entries = Vec::with_capacity(template.len());
    for entry in template {
        match entry {
            InterpretedClosureEnvEntry::TopLevelSentinel => entries.push(Value::T),
            InterpretedClosureEnvEntry::Special(sym) => entries.push(Value::from_sym_id(*sym)),
            InterpretedClosureEnvEntry::Binding(sym) => {
                let cell = lexenv_assq(source_env, *sym)
                    .expect("cached interpreted-closure env binding should exist");
                entries.push(cell);
            }
        }
    }
    Value::list(entries)
}

#[derive(Clone, Debug)]
enum NamedCallTarget {
    Obarray(Value),
    ContextCallable,
    Builtin,
    SpecialForm,
    Void,
}

#[derive(Clone, Debug)]
struct NamedCallCache {
    symbol: SymId,
    function_epoch: u64,
    target: NamedCallTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum InterpretedClosureEnvEntry {
    TopLevelSentinel,
    Special(SymId),
    Binding(SymId),
}

#[derive(Clone, Debug)]
pub(crate) struct MacroExpansionCacheEntry {
    expanded: Rc<Expr>,
    fingerprint: u64,
}

impl MacroExpansionCacheEntry {
    fn new(expanded: Rc<Expr>, fingerprint: u64) -> Self {
        Self {
            expanded,
            fingerprint,
        }
    }
}

#[derive(Clone, Debug)]
struct InterpretedClosureTrimCacheEntry {
    params_expr: Expr,
    body_exprs: Vec<Expr>,
    iform_expr: Expr,
    env_shape: Vec<InterpretedClosureEnvEntry>,
    params: LambdaParams,
    trimmed_body: Rc<Vec<Expr>>,
    trimmed_env_template: Vec<InterpretedClosureEnvEntry>,
}

impl InterpretedClosureTrimCacheEntry {
    fn matches(
        &self,
        params_expr: &Expr,
        body_exprs: &[Expr],
        iform_expr: &Expr,
        env_shape: &[InterpretedClosureEnvEntry],
    ) -> bool {
        self.params_expr == *params_expr
            && self.body_exprs == body_exprs
            && self.iform_expr == *iform_expr
            && self.env_shape == env_shape
    }
}

fn value_from_symbol_id(sym_id: SymId) -> Value {
    let name = resolve_sym(sym_id);
    if lookup_interned(name).is_some_and(|canonical| canonical == sym_id) {
        if name == "nil" {
            return Value::NIL;
        }
        if name == "t" {
            return Value::T;
        }
        if name.starts_with(':') {
            return Value::from_kw_id(sym_id);
        }
    }
    Value::from_sym_id(sym_id)
}

fn is_runtime_dynamically_special(obarray: &Obarray, sym_id: SymId) -> bool {
    obarray.is_special_id(sym_id) && !obarray.is_constant_id(sym_id)
}

fn symbol_sets_constant_error(sym_id: SymId) -> Option<&'static str> {
    match resolve_sym(sym_id) {
        "nil" => Some("nil"),
        "t" => Some("t"),
        _ => None,
    }
}

pub(crate) fn sync_features_variable_in_state(obarray: &mut Obarray, features: &[SymId]) {
    let values: Vec<Value> = features.iter().map(|id| Value::from_sym_id(*id)).collect();
    obarray.set_symbol_value("features", Value::list(values));
}

pub(crate) fn refresh_features_from_variable_in_state(
    obarray: &Obarray,
    features: &mut Vec<SymId>,
) {
    let current = obarray
        .symbol_value("features")
        .cloned()
        .unwrap_or(Value::NIL);
    let mut parsed = Vec::new();
    if let Some(items) = list_to_vec(&current) {
        for item in items {
            if let Some(id) = item.as_symbol_id() {
                parsed.push(id);
            }
        }
    }
    *features = parsed;
}

pub(crate) fn feature_present_in_state(
    obarray: &Obarray,
    features: &mut Vec<SymId>,
    name: &str,
) -> bool {
    refresh_features_from_variable_in_state(obarray, features);
    let id = intern(name);
    features.iter().any(|feature| *feature == id)
}

pub(crate) fn add_feature_in_state(obarray: &mut Obarray, features: &mut Vec<SymId>, name: &str) {
    refresh_features_from_variable_in_state(obarray, features);
    let id = intern(name);
    if features.iter().any(|feature| *feature == id) {
        return;
    }
    // Emacs pushes newly-provided features at the front.
    features.insert(0, id);
    sync_features_variable_in_state(obarray, features);
}

pub(crate) fn remove_feature_in_state(
    obarray: &mut Obarray,
    features: &mut Vec<SymId>,
    name: &str,
) {
    refresh_features_from_variable_in_state(obarray, features);
    let id = intern(name);
    features.retain(|feature| *feature != id);
    sync_features_variable_in_state(obarray, features);
}

pub(crate) fn provide_value_in_state(
    obarray: &mut Obarray,
    features: &mut Vec<SymId>,
    feature: Value,
    subfeatures: Option<Value>,
) -> EvalResult {
    let name = match feature.kind() {
        ValueKind::Symbol(symbol) => resolve_sym(symbol).to_owned(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), feature],
            ));
        }
    };
    if let Some(value) = subfeatures {
        obarray.put_property(&name, "subfeatures", value);
    }
    add_feature_in_state(obarray, features, &name);
    Ok(feature)
}

/// Limit for stored recent input events to match GNU Emacs: 300 entries.
pub(crate) const RECENT_INPUT_EVENT_LIMIT: usize = 300;

thread_local! {
    static SCRATCH_GC_ROOTS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

/// Collect GC roots from all thread-local statics that hold Values.
///
/// Thread-local statics are invisible to the normal GC root scan (which
/// only walks the Evaluator struct and its sub-managers).  This function
/// calls each module's `collect_*_gc_roots` helper to ensure those Values
/// are marked as live during garbage collection.
fn collect_thread_local_gc_roots(roots: &mut Vec<Value>) {
    super::value::collect_string_text_prop_gc_roots(roots);
    super::syntax::collect_syntax_gc_roots(roots);
    super::casetab::collect_casetab_gc_roots(roots);
    super::category::collect_category_gc_roots(roots);
    super::terminal::pure::collect_terminal_gc_roots(roots);
    super::font::collect_font_gc_roots(roots);
    super::ccl::collect_ccl_gc_roots(roots);
    SCRATCH_GC_ROOTS.with(|scratch| roots.extend(scratch.borrow().iter().copied()));
}

pub fn save_scratch_gc_roots() -> usize {
    SCRATCH_GC_ROOTS.with(|scratch| scratch.borrow().len())
}

pub fn push_scratch_gc_root(value: Value) {
    SCRATCH_GC_ROOTS.with(|scratch| scratch.borrow_mut().push(value));
}

pub fn restore_scratch_gc_roots(saved_len: usize) {
    SCRATCH_GC_ROOTS.with(|scratch| scratch.borrow_mut().truncate(saved_len));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiFrameHostRequest {
    pub frame_id: crate::window::FrameId,
    pub width: u32,
    pub height: u32,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuiFrameHostSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct FontResolveRequest {
    pub frame_id: crate::window::FrameId,
    pub character: char,
    pub face: RuntimeFace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSpecResolveRequest {
    pub frame_id: crate::window::FrameId,
    pub family: Option<String>,
    pub registry: Option<String>,
    pub lang: Option<String>,
    pub weight: Option<FontWeight>,
    pub slant: Option<FontSlant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFontMatch {
    pub family: String,
    pub foundry: Option<String>,
    pub weight: FontWeight,
    pub slant: FontSlant,
    pub width: FontWidth,
    pub postscript_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFontSpecMatch {
    pub family: String,
    pub registry: Option<String>,
    pub weight: Option<FontWeight>,
    pub slant: Option<FontSlant>,
    pub width: Option<FontWidth>,
    pub spacing: Option<i32>,
    pub postscript_name: Option<String>,
}

pub trait DisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String>;
    fn resize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String>;
    fn opening_gui_frame_pending(&self) -> bool {
        false
    }
    fn current_primary_window_size(&self) -> Option<GuiFrameHostSize> {
        None
    }
    fn resolve_font_for_char(
        &mut self,
        _request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        Ok(None)
    }
    fn resolve_font_for_spec(
        &mut self,
        _request: FontSpecResolveRequest,
    ) -> Result<Option<ResolvedFontSpecMatch>, String> {
        Ok(None)
    }
}

/// The Elisp evaluator.
///
/// # Safety: Send
/// Evaluator is inherently single-threaded (uses thread-local heap + interner).
/// # Safety: Send
/// Context is inherently single-threaded (uses thread-local heap + interner).
/// `neovm-worker` moves the Context to a worker thread inside
/// `Arc<Mutex<..>>`, which ensures exclusive access.
// SAFETY: Rc is !Send only because it uses non-atomic refcounting.
// Since Context is always used single-threaded (guarded by Mutex when
// transferred between threads), this is safe.
unsafe impl Send for Context {}

/// A registered builtin function (equivalent to GNU Emacs's Lisp_Subr).
///
/// Each SubrObject stores a function pointer that takes `&mut Context` plus
/// evaluated arguments, matching GNU Emacs's `defsubr` registration model.
pub struct SubrObject {
    pub function: fn(&mut Context, Vec<Value>) -> EvalResult,
    pub min_args: u16,
    /// None means MANY (&rest).
    pub max_args: Option<u16>,
    pub name: SymId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ResumeTarget {
    CommandLoopExit,
    CommandLoopTopLevel,
    InterpreterCatch,
    InterpreterConditionCase {
        handler_index: usize,
        condition_stack_base: usize,
    },
    VmCatch {
        resume_id: u64,
        target: u32,
        stack_len: usize,
        spec_depth: usize,
    },
    VmConditionCase {
        resume_id: u64,
        target: u32,
        stack_len: usize,
        spec_depth: usize,
    },
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum ConditionFrame {
    Catch {
        tag: Value,
        resume: ResumeTarget,
    },
    ConditionCase {
        conditions: Value,
        resume: ResumeTarget,
    },
    HandlerBind {
        conditions: Value,
        handler: Value,
        mute_span: usize,
    },
    SkipConditions {
        remaining: usize,
    },
}

fn condition_value_contains_debug(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Symbol(id) => resolve_sym(id) == "debug",
        ValueKind::Cons => {
            list_to_vec(value).is_some_and(|items| items.iter().any(condition_value_contains_debug))
        }
        _ => false,
    }
}

fn wants_debugger(setting: &Value, conditions: &Value) -> bool {
    if setting.is_nil() {
        return false;
    }
    let Some(entries) = list_to_vec(setting) else {
        return true;
    };
    let signal_conditions = list_to_vec(conditions).unwrap_or_else(|| vec![*conditions]);
    entries
        .iter()
        .any(|entry| signal_conditions.iter().any(|condition| condition == entry))
}

fn signal_hook_payload_value(sig: &SignalData) -> Value {
    if let Some(raw) = &sig.raw_data {
        *raw
    } else if sig.data.is_empty() {
        Value::NIL
    } else {
        Value::list(sig.data.clone())
    }
}

pub struct Context {
    /// Builtin function registry — directly indexed by `SymId`.
    ///
    /// GNU Emacs dispatches subrs from the function object itself rather than
    /// paying an extra hash-table lookup. NeoVM's function cells still store
    /// `Value::subr(sym_id)`, but the registry backing that value is a direct
    /// slot table so builtin dispatch follows symbol identity, not name hashing.
    pub(crate) subr_registry: Vec<Option<SubrObject>>,
    /// String interner for symbol/keyword/subr names (SymId handles).
    pub(crate) interner: Box<StringInterner>,
    /// Tagged pointer heap — sole GC and allocator.
    pub(crate) tagged_heap: Box<crate::tagged::gc::TaggedHeap>,
    /// The obarray — unified symbol table with value cells, function cells, plists.
    pub(crate) obarray: Obarray,
    /// Specpdl — special binding stack that writes directly to the obarray.
    /// Matches GNU Emacs's specpdl design.
    pub(crate) specpdl: Vec<SpecBinding>,
    /// Lexical environment: flat cons alist mirroring GNU Emacs's
    /// `Vinternal_interpreter_environment`.
    pub(crate) lexenv: Value,
    /// GNU `eval.c` keeps `Vinternal_interpreter_environment` on a hidden
    /// symbol object by `Funintern`ing the public name from the obarray.
    /// NeoVM keeps the actual evaluator-owned symbol identity here so the
    /// public `internal-interpreter-environment` symbol can stay visible
    /// while remaining unbound and non-special.
    pub(crate) internal_interpreter_environment_symbol: SymId,
    /// GNU `eval.c` hot-path DEFVARs exposed via direct globals like
    /// `Vquit_flag`, `Vinhibit_quit`, and `Vthrow_on_input`.
    ///
    /// NeoVM still stores their values in the obarray's symbol cells so Lisp
    /// sees ordinary variables, but evaluator boundaries keep their symbol
    /// identities cached here to avoid repeated name interning/lookups.
    quit_flag_symbol: SymId,
    inhibit_quit_symbol: SymId,
    throw_on_input_symbol: SymId,
    kill_emacs_symbol: SymId,
    /// Features list (for require/provide).
    pub(crate) features: Vec<SymId>,
    /// Features currently being resolved through `require`.
    pub(crate) require_stack: Vec<SymId>,
    /// Files currently being loaded (mirrors `Vloads_in_progress` in lread.c).
    pub(crate) loads_in_progress: Vec<std::path::PathBuf>,
    /// Buffer manager — owns all live buffers and tracks current buffer.
    pub buffers: BufferManager,
    /// Match data from the last successful search/match operation.
    pub(crate) match_data: Option<MatchData>,
    /// Process manager — owns all tracked processes.
    pub(crate) processes: ProcessManager,
    /// Network manager — owns network connections, filters, and sentinels.
    /// Timer manager — owns all timers.
    pub(crate) timers: TimerManager,
    /// Variable watcher list — callbacks on variable changes.
    pub(crate) watchers: VariableWatcherList,
    /// Canonical Lisp object returned by `standard-syntax-table`.
    ///
    /// GNU Emacs stores this in `Vstandard_syntax_table`; NeoVM keeps the
    /// authoritative identity here and mirrors it into thread-local state for
    /// no-evaluator syntax builtins.
    pub(crate) standard_syntax_table: Value,
    /// Canonical Lisp object returned by `standard-category-table`.
    ///
    /// Like `standard_syntax_table`, this is mirrored into thread-local state
    /// because the category-table helpers currently expose some no-evaluator
    /// entry points.
    pub(crate) standard_category_table: Value,
    /// Current buffer-local keymap (set by `use-local-map`).
    pub(crate) current_local_map: Value,
    /// Register manager — quick storage and retrieval of text, positions, etc.
    pub(crate) registers: RegisterManager,
    /// Bookmark manager — persistent named positions.
    pub(crate) bookmarks: BookmarkManager,
    /// Abbreviation manager — text abbreviation expansion.
    pub(crate) abbrevs: AbbrevManager,
    /// Autoload manager — deferred function loading.
    pub(crate) autoloads: AutoloadManager,
    /// Custom variable manager — defcustom/defgroup system.
    pub(crate) custom: CustomManager,
    /// Rectangle state — stores the last killed rectangle for yank-rectangle.
    pub(crate) rectangle: RectangleState,
    /// Interactive command registry — tracks interactive commands.
    pub(crate) interactive: InteractiveRegistry,
    /// Minibuffer runtime state — active minibuffer stack, prompt metadata, and history.
    pub(crate) minibuffers: MinibufferManager,
    /// Current echo-area message text, mirroring GNU `current-message`.
    current_message: Option<String>,
    /// Window that was selected when the active minibuffer session began.
    pub(crate) minibuffer_selected_window: Option<crate::window::WindowId>,
    /// Currently active minibuffer window, if any.
    pub(crate) active_minibuffer_window: Option<crate::window::WindowId>,
    /// Pending orderly shutdown requested by GNU C-owned primitives such as
    /// `kill-emacs`.
    pub(crate) shutdown_request: Option<ShutdownRequest>,
    /// Batch-compatible input-mode interrupt flag for `current-input-mode`.
    pub(crate) input_mode_interrupt: bool,
    /// Lisp-visible `quit_char` used by `current-input-mode` and low-level
    /// keyboard quit detection.
    pub(crate) quit_char: i64,
    /// True while the command loop is blocked waiting for external input.
    pub(crate) waiting_for_user_input: bool,
    /// Frame manager — owns all frames and windows.
    pub(crate) frames: FrameManager,
    /// Mode registry — major/minor modes.
    pub(crate) modes: ModeRegistry,
    /// Thread manager — cooperative threading primitives.
    pub(crate) threads: ThreadManager,
    /// Keyboard macro metadata — ring/counter state layered above the
    /// keyboard-owned live recording/playback runtime.
    pub(crate) kmacro: KmacroManager,
    /// Command loop state — event queue, prefix args, kbd macros, quit flag.
    /// Used by the interactive command loop (recursive-edit → command_loop).
    pub(crate) command_loop: crate::keyboard::CommandLoop,
    /// Input event receiver from the display/render thread.
    /// `None` in batch mode (tests, non-interactive evaluation).
    /// When `Some`, `read_char()` blocks on this channel for interactive input.
    pub input_rx: Option<crossbeam_channel::Receiver<crate::keyboard::InputEvent>>,
    /// Wakeup file descriptor — the read end of a pipe that the render thread
    /// writes to when input is available.  Used by `wait_for_input()` with
    /// `pselect()`/`poll()` to multiplex input with process I/O and timers.
    /// `None` in batch mode.
    #[cfg(unix)]
    pub wakeup_fd: Option<std::os::unix::io::RawFd>,
    /// Redisplay callback — called before blocking for input in `read_char()`.
    ///
    /// In GNU Emacs, `read_char()` calls `redisplay()` directly (keyboard.c
    /// calls xdisp.c, both in the same binary). In our crate structure,
    /// `neomacs-layout-engine` depends on `neovm-core`, so neovm-core cannot
    /// call the layout engine directly (circular dependency). Instead,
    /// `neomacs-bin` sets this callback to run the layout engine and send
    /// the resulting `FrameGlyphBuffer` to the render thread.
    ///
    /// `None` in batch mode (no display).
    pub redisplay_fn: Option<Box<dyn FnMut(&mut Self)>>,
    /// Host-display bridge for GUI frame realization.
    pub display_host: Option<Box<dyn DisplayHost>>,
    /// Coding system manager — encoding/decoding registry.
    pub(crate) coding_systems: CodingSystemManager,
    /// Face table — global registry of named face definitions.
    pub(crate) face_table: FaceTable,
    /// Incremented when any face attribute changes; layout engine uses
    /// this to invalidate its resolved face cache.
    pub face_change_count: u64,
    /// Recursion depth counter.
    pub(crate) depth: usize,
    eval_counter: u64,
    /// Maximum recursion depth.
    pub(crate) max_depth: usize,
    /// Set when allocation crosses the GC threshold; cleared by `gc_collect`.
    pub(crate) gc_pending: bool,
    /// Total number of GC collections performed.
    pub(crate) gc_count: u64,
    /// Nested depth of explicit GC inhibition scopes.
    pub(crate) gc_inhibit_depth: usize,
    /// Stress-test mode: force GC at every safe point regardless of threshold.
    pub(crate) gc_stress: bool,
    /// Temporary GC roots — Values that must survive collection but aren't
    /// in any other rooted structure (e.g. intermediate results in eval_forms).
    temp_roots: Vec<Value>,
    /// VM GC roots — Values that must remain GC-visible while the bytecode VM
    /// crosses into evaluator code that may trigger collection.
    pub(crate) vm_gc_roots: Vec<Value>,
    /// GNU-shaped Lisp call stack used by `backtrace-frame--internal`,
    /// `mapbacktrace`, and advice-sensitive `called-interactively-p`.
    pub(crate) runtime_backtrace: Vec<RuntimeBacktraceFrame>,
    /// Shared condition runtime mirror for active catch/condition handlers.
    pub(crate) condition_stack: Vec<ConditionFrame>,
    /// Stable identity source for VM resume targets stored in the shared
    /// condition runtime.
    next_resume_id: u64,
    /// GNU `pending_funcalls` equivalent for internal no-Lisp teardown paths.
    pub(crate) pending_safe_funcalls: Vec<PendingSafeFuncall>,
    /// Saved lexical environments stack — when apply_lambda replaces
    /// self.lexenv with a closure's captured env, the old lexenv is pushed
    /// here so GC can still scan it.  Popped when apply_lambda restores.
    saved_lexenvs: Vec<Value>,
    /// Small hot cache for named callable resolution in `funcall`/`apply`.
    named_call_cache: Vec<NamedCallCache>,
    /// Cache for source-literal materialization keyed on `Expr` pointer
    /// identity. This is generic reader/runtime support: repeated evaluation
    /// of the same source literal node should reuse the same runtime object
    /// where GNU Lisp expects `eq` identity to remain stable across calls.
    /// GC-rooted via `collect_roots`.
    pub(crate) source_literal_cache: HashMap<*const Expr, Value>,
    /// Cache for macro expansion results.
    ///
    /// Key: `(macro_heap_id, args_slice_ptr)` — the macro's tagged pointer plus the
    /// pointer to the args `&[Expr]` slice.
    ///
    /// Value: `(Rc<Expr>, u64)` — the expanded Expr tree plus a content
    /// fingerprint of the args at insertion time.  On cache hit, the
    /// fingerprint is recomputed and compared to detect ABA: when a
    /// lambda body `Rc<Vec<Expr>>` is freed during macro expansion (e.g.
    /// temporary lambdas in pcase), its memory can be reused by a new
    /// lambda body, making `tail.as_ptr()` match a stale entry whose
    /// args are completely different.  The fingerprint catches this.
    pub(crate) macro_expansion_cache: HashMap<(usize, usize, u64), Rc<MacroExpansionCacheEntry>>,
    /// Diagnostic counters for macro expansion cache.
    pub(crate) macro_cache_hits: u64,
    pub(crate) macro_cache_misses: u64,
    pub(crate) macro_expand_total_us: u64,
    /// When true, skip cache lookups (still populate cache for timing).
    pub(crate) macro_cache_disabled: bool,
    /// Bootstrapped standard interpreted-closure filter function object.
    /// Used to memoize the GNU cconv closure-trimming path without changing
    /// semantics when users later rebind/advice the hook.
    interpreted_closure_filter_fn: Option<Value>,
    /// Cache of standard cconv interpreted-closure trimming results keyed by
    /// lambda syntax plus lexical-environment shape. The cached data stores
    /// only the selected env template and trimmed body, so captured values are
    /// always rebuilt from the current runtime environment on a hit.
    interpreted_closure_trim_cache: HashMap<u64, Vec<InterpretedClosureTrimCacheEntry>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownRequest {
    pub exit_code: i32,
    pub restart: bool,
}

pub(crate) enum RequirePlan {
    Return(Value),
    Load {
        sym_id: SymId,
        name: String,
        path: std::path::PathBuf,
    },
}

pub(crate) fn plan_require_in_state(
    obarray: &Obarray,
    features: &mut Vec<SymId>,
    require_stack: &[SymId],
    feature: Value,
    filename: Option<Value>,
    noerror: Option<Value>,
) -> Result<RequirePlan, Flow> {
    refresh_features_from_variable_in_state(obarray, features);
    let sym_id = match feature.kind() {
        ValueKind::Symbol(s) => s,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), feature],
            ));
        }
    };
    let name = resolve_sym(sym_id).to_owned();
    if features.contains(&sym_id) {
        return Ok(RequirePlan::Return(Value::symbol(&name)));
    }

    // Preserve current NeoVM recursive-require semantics in this bridge-slice.
    if require_stack.contains(&sym_id) {
        tracing::debug!(
            "Recursive require for feature '{}', returning immediately",
            name
        );
        return Ok(RequirePlan::Return(Value::symbol(&name)));
    }

    let filename = match filename {
        Some(v) if v.is_nil() => name.clone(),
        Some(v) if v.is_string() => v.as_str().unwrap().to_owned(),
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), other],
            ));
        }
        None => name.clone(),
    };
    let filename = super::load::expand_tilde(&filename);

    let load_path = super::load::get_load_path(obarray);
    match super::load::find_file_in_load_path(&filename, &load_path) {
        Some(path) => Ok(RequirePlan::Load { sym_id, name, path }),
        None => {
            if noerror.is_some_and(|value| value.is_truthy()) {
                return Ok(RequirePlan::Return(Value::NIL));
            }
            Err(signal(
                "file-missing",
                vec![Value::string(format!(
                    "Cannot open load file: no such file or directory, {}",
                    name
                ))],
            ))
        }
    }
}

pub(crate) fn finish_require_in_state(features: &[SymId], sym_id: SymId, name: &str) -> EvalResult {
    if features.contains(&sym_id) {
        Ok(Value::symbol(name))
    } else {
        Err(signal(
            "error",
            vec![Value::string(format!(
                "Required feature '{}' was not provided",
                name
            ))],
        ))
    }
}

pub(crate) fn builtin_require_in_vm_runtime(
    shared: &mut Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    match plan_require_in_state(
        &shared.obarray,
        &mut shared.features,
        &shared.require_stack,
        args.first().copied().unwrap_or(Value::NIL),
        args.get(1).copied(),
        args.get(2).copied(),
    )? {
        RequirePlan::Return(value) => Ok(value),
        RequirePlan::Load { sym_id, name, path } => {
            shared.require_stack.push(sym_id);
            let extra_roots = args.to_vec();
            let result = shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
                eval.load_file_internal(&path)
            });
            let _ = shared.require_stack.pop();
            result?;
            refresh_features_from_variable_in_state(&shared.obarray, &mut shared.features);
            finish_require_in_state(&shared.features, sym_id, &name)
        }
    }
}

/// VM-side `provide` that delegates to the parent evaluator so that
/// `after-load-alist` callbacks are executed (matching GNU's Fprovide).
pub(crate) fn builtin_provide_in_vm_runtime(
    shared: &mut Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    if args.is_empty() || args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("provide"), Value::fixnum(args.len() as i64)],
        ));
    }
    let feature = args[0];
    let subfeatures = args.get(1).copied();
    let extra_roots = args.to_vec();
    shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
        eval.provide_value(feature, subfeatures)
    })
}

pub(crate) fn parse_eval_lexical_arg(arg: Option<Value>) -> Result<(bool, Option<Value>), Flow> {
    let Some(arg) = arg else {
        return Ok((false, None));
    };
    if arg.is_nil() {
        return Ok((false, None));
    }

    // GNU eval.c:
    //   specbind(Qinternal_interpreter_environment,
    //            CONSP(lexical) || NILP(lexical) ? lexical : list_of_t);
    // - non-nil atom (like t) => lexical mode, env = (t)  [the list!]
    // - cons                  => lexical mode, env = lexical
    if !arg.is_cons() {
        // Non-nil non-cons: set env to (t) matching GNU's list_of_t
        return Ok((true, Some(Value::list(vec![Value::T]))));
    };

    if list_to_vec(&arg).is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), arg],
        ));
    }

    Ok((true, Some(arg)))
}

fn lexical_binding_in_obarray(obarray: &Obarray) -> bool {
    obarray
        .symbol_value("lexical-binding")
        .is_some_and(|v| v.is_truthy())
}

pub(crate) struct ActiveEvalLexicalArgState {
    saved_lexical_mode: bool,
    has_saved_lexenv: bool,
}

pub(crate) fn begin_eval_with_lexical_arg_in_state(
    obarray: &mut Obarray,
    lexenv: &mut Value,
    saved_lexenvs: &mut Vec<Value>,
    lexical_arg: Option<Value>,
) -> Result<ActiveEvalLexicalArgState, Flow> {
    let (use_lexical, lexenv_value) = parse_eval_lexical_arg(lexical_arg)?;
    let saved_lexical_mode = lexical_binding_in_obarray(obarray);
    obarray.set_symbol_value("lexical-binding", Value::bool_val(use_lexical));
    let has_saved_lexenv = if let Some(env) = lexenv_value {
        saved_lexenvs.push(*lexenv);
        *lexenv = env;
        true
    } else {
        false
    };
    Ok(ActiveEvalLexicalArgState {
        saved_lexical_mode,
        has_saved_lexenv,
    })
}

pub(crate) fn finish_eval_with_lexical_arg_in_state(
    obarray: &mut Obarray,
    lexenv: &mut Value,
    saved_lexenvs: &mut Vec<Value>,
    state: ActiveEvalLexicalArgState,
) {
    if state.has_saved_lexenv {
        *lexenv = saved_lexenvs.pop().expect("saved_lexenvs underflow");
    }
    obarray.set_symbol_value("lexical-binding", Value::bool_val(state.saved_lexical_mode));
}

pub(crate) fn builtin_eval_in_vm_runtime(
    shared: &mut Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    if !(1..=2).contains(&args.len()) {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("eval"), Value::fixnum(args.len() as i64)],
        ));
    }

    let form = args[0];
    let lexical_arg = args.get(1).copied();
    let state = shared.begin_eval_with_lexical_arg(lexical_arg)?;
    let result = shared.with_extra_gc_roots(vm_gc_roots, args, move |eval| eval.eval_value(&form));
    shared.finish_eval_with_lexical_arg(state);
    result
}

pub(crate) fn eval_lambda_body_in_vm_runtime(
    shared: &mut Context,
    vm_gc_roots: &[Value],
    extra_roots: &[Value],
    body: Rc<Vec<Expr>>,
) -> EvalResult {
    shared.with_extra_gc_roots(vm_gc_roots, extra_roots, move |eval| {
        eval.eval_lambda_body(&body)
    })
}

pub(crate) struct ActiveLambdaCallState {
    saved_temp_roots_len: usize,
    has_lexenv: bool,
    saved_lexical_mode: Option<bool>,
    specpdl_count: usize,
}

pub(crate) struct ActiveMacroExpansionScopeState {
    saved_temp_roots_len: usize,
    old_lexical: bool,
    old_dynvars: Value,
}

fn bind_lexical_value_rooted_in_state(
    lexenv: &mut Value,
    temp_roots: &mut Vec<Value>,
    sym: SymId,
    value: Value,
) {
    let saved_roots = temp_roots.len();
    temp_roots.push(value);
    *lexenv = lexenv_prepend(*lexenv, sym, value);
    temp_roots.truncate(saved_roots);
}

/// Build a `(MIN . MAX)` cons cell representing the arity of a lambda/closure,
/// matching the format GNU Emacs uses in `wrong-number-of-arguments` errors.
/// `MAX` is the symbol `many` when the function accepts `&rest`.
fn lambda_arity_cons(params: &LambdaParams) -> Value {
    let min_val = Value::fixnum(params.min_arity() as i64);
    let max_val = match params.max_arity() {
        Some(n) => Value::fixnum(n as i64),
        None => Value::symbol("many"),
    };
    Value::cons(min_val, max_val)
}

fn begin_lambda_call_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    lexenv: &mut Value,
    saved_lexenvs: &mut Vec<Value>,
    temp_roots: &mut Vec<Value>,
    lambda: &LambdaData,
    args: &[Value],
    func_value: Value,
) -> Result<ActiveLambdaCallState, Flow> {
    let params = &lambda.params;

    if args.len() < params.min_arity() {
        tracing::warn!(
            "wrong-number-of-arguments (lambda call too few): got {} args, min={}, params={:?}, docstring={:?}",
            args.len(),
            params.min_arity(),
            params,
            lambda.docstring
        );
        let arity_val = lambda_arity_cons(params);
        return Err(signal(
            "wrong-number-of-arguments",
            vec![arity_val, Value::fixnum(args.len() as i64)],
        ));
    }
    if let Some(max) = params.max_arity()
        && args.len() > max
    {
        let arity_val = lambda_arity_cons(params);
        return Err(signal(
            "wrong-number-of-arguments",
            vec![arity_val, Value::fixnum(args.len() as i64)],
        ));
    }

    let saved_temp_roots_len = temp_roots.len();
    let specpdl_count = specpdl.len();
    temp_roots.extend(args.iter().copied());

    let has_lexenv = lambda.env.is_some();
    if let Some(env) = lambda.env {
        // Debug: detect malformed env (bare t instead of list (t))
        if env.is_t() {
            tracing::error!(
                "Lambda called with env=t (should be (t))! params={:?}",
                lambda.params
            );
        }
        temp_roots.push(env);
        let old = std::mem::replace(lexenv, env);
        temp_roots.push(old);
        saved_lexenvs.push(old);

        let mut arg_idx = 0;
        for param in &params.required {
            bind_lexical_value_rooted_in_state(lexenv, temp_roots, *param, args[arg_idx]);
            arg_idx += 1;
        }
        for param in &params.optional {
            if arg_idx < args.len() {
                bind_lexical_value_rooted_in_state(lexenv, temp_roots, *param, args[arg_idx]);
                arg_idx += 1;
            } else {
                bind_lexical_value_rooted_in_state(lexenv, temp_roots, *param, Value::NIL);
            }
        }
        if let Some(rest_name) = params.rest {
            let rest_value = Value::list(args[arg_idx..].to_vec());
            bind_lexical_value_rooted_in_state(lexenv, temp_roots, rest_name, rest_value);
        }
    } else {
        // Dynamic binding: use specbind to write directly to obarray.
        let mut arg_idx = 0;
        for param in &params.required {
            specbind_in_state(obarray, specpdl, *param, args[arg_idx]);
            arg_idx += 1;
        }
        for param in &params.optional {
            if arg_idx < args.len() {
                specbind_in_state(obarray, specpdl, *param, args[arg_idx]);
                arg_idx += 1;
            } else {
                specbind_in_state(obarray, specpdl, *param, Value::NIL);
            }
        }
        if let Some(rest_name) = params.rest {
            specbind_in_state(
                obarray,
                specpdl,
                rest_name,
                Value::list(args[arg_idx..].to_vec()),
            );
        }
    }

    let saved_lexical_mode = if has_lexenv {
        let old = obarray
            .symbol_value("lexical-binding")
            .is_some_and(|value| value.is_truthy());
        obarray.set_symbol_value("lexical-binding", Value::T);
        Some(old)
    } else {
        None
    };

    Ok(ActiveLambdaCallState {
        saved_temp_roots_len,
        has_lexenv,
        saved_lexical_mode,
        specpdl_count,
    })
}

fn finish_lambda_call_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    lexenv: &mut Value,
    saved_lexenvs: &mut Vec<Value>,
    temp_roots: &mut Vec<Value>,
    state: ActiveLambdaCallState,
) {
    if let Some(old_mode) = state.saved_lexical_mode {
        obarray.set_symbol_value("lexical-binding", Value::bool_val(old_mode));
    }
    if state.has_lexenv {
        let old_lexenv = saved_lexenvs.pop().expect("saved_lexenvs underflow");
        *lexenv = old_lexenv;
    } else {
        unbind_to_in_state(obarray, specpdl, state.specpdl_count);
    }
    temp_roots.truncate(state.saved_temp_roots_len);
}

fn begin_macro_expansion_scope_in_state(
    obarray: &mut Obarray,
    specpdl: &[SpecBinding],
    buffers: &mut BufferManager,
    custom: &CustomManager,
    lexenv: Value,
    temp_roots: &mut Vec<Value>,
) -> ActiveMacroExpansionScopeState {
    let saved_temp_roots_len = temp_roots.len();
    let old_lexical = obarray
        .symbol_value("lexical-binding")
        .is_some_and(|value| value.is_truthy());
    let old_dynvars = obarray
        .symbol_value("macroexp--dynvars")
        .cloned()
        .unwrap_or(Value::NIL);
    temp_roots.push(old_dynvars);

    let mut dynvars = old_dynvars;
    for sym in lexenv_bare_symbols(lexenv) {
        let name = resolve_sym(sym);
        if name == "t" || name == "nil" {
            continue;
        }
        dynvars = Value::cons(Value::from_sym_id(sym), dynvars);
    }
    // Collect symbols from the specpdl (replaces dynamic frame iteration).
    for entry in specpdl.iter().rev() {
        let sym_id = match entry {
            SpecBinding::Let { sym_id, .. }
            | SpecBinding::LetLocal { sym_id, .. }
            | SpecBinding::LetDefault { sym_id, .. } => sym_id,
        };
        let name = resolve_sym(*sym_id);
        if name == "t" || name == "nil" {
            continue;
        }
        dynvars = Value::cons(Value::from_sym_id(*sym_id), dynvars);
    }

    obarray.set_symbol_value("lexical-binding", Value::bool_val(!lexenv.is_nil()));
    set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("macroexp--dynvars"),
        dynvars,
    );

    ActiveMacroExpansionScopeState {
        saved_temp_roots_len,
        old_lexical,
        old_dynvars,
    }
}

fn finish_macro_expansion_scope_in_state(
    obarray: &mut Obarray,
    specpdl: &[SpecBinding],
    buffers: &mut BufferManager,
    custom: &CustomManager,
    temp_roots: &mut Vec<Value>,
    state: ActiveMacroExpansionScopeState,
) {
    set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("macroexp--dynvars"),
        state.old_dynvars,
    );
    obarray.set_symbol_value("lexical-binding", Value::bool_val(state.old_lexical));
    temp_roots.truncate(state.saved_temp_roots_len);
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if std::ptr::eq(current_interner_ptr(), &mut *self.interner) {
            clear_current_interner();
        }
    }
}

impl Context {
    #[inline]
    pub(crate) fn subr_slot(&self, sym_id: SymId) -> Option<&SubrObject> {
        self.subr_registry
            .get(sym_id.0 as usize)
            .and_then(|slot| slot.as_ref())
    }

    #[inline]
    fn has_registered_subr(&self, sym_id: SymId) -> bool {
        self.subr_slot(sym_id).is_some()
    }

    fn register_subr_slot(&mut self, sym_id: SymId, subr: SubrObject) {
        let index = sym_id.0 as usize;
        if self.subr_registry.len() <= index {
            self.subr_registry.resize_with(index + 1, || None);
        }
        self.subr_registry[index] = Some(subr);
    }

    pub fn new() -> Self {
        let mut ctx = Self::new_inner(true);
        // Capture stack bottom for conservative GC stack scanning.
        // Read the thread's stack upper bound from /proc/self/maps.
        #[cfg(target_os = "linux")]
        {
            if let Some(stack_end) = crate::tagged::gc::read_stack_end_from_proc() {
                ctx.tagged_heap.set_stack_bottom(stack_end as *const u8);
            }
        }
        // Register builtins AFTER new_inner returns — the function is too
        // large (1500+ lines) for reliable codegen in debug mode when
        // combined with init_builtins (1162 defsubr calls in the same frame).
        builtins::init_builtins(&mut ctx);
        ctx
    }

    #[cfg(test)]
    pub(crate) fn new_vm_runtime_harness() -> Self {
        // GNU bytecode executes inside the same callable runtime surface as the
        // ordinary evaluator. Keep the default VM harness on that full surface.
        Self::new()
    }

    #[cfg(test)]
    pub(crate) fn new_minimal_vm_harness() -> Self {
        // Keep this reduced constructor only for low-level VM/opcode tests
        // that intentionally do not depend on the full builtin surface.
        let mut ev = Self::new_inner(true);
        ev.obarray = Obarray::new();
        super::errors::init_standard_errors(&mut ev.obarray);
        ev.obarray
            .set_symbol_value("most-positive-fixnum", Value::fixnum(i64::MAX >> 2));
        ev.obarray
            .set_symbol_value("most-negative-fixnum", Value::fixnum(-(i64::MAX >> 2) - 1));
        ev.specpdl.clear();
        ev.lexenv = Value::NIL;
        ev.features.clear();
        ev.require_stack.clear();
        ev.loads_in_progress.clear();
        ev.buffers = BufferManager::new();
        ev.match_data = None;
        ev.processes = ProcessManager::new();
        ev.timers = TimerManager::new();
        ev.watchers = VariableWatcherList::new();
        ev.current_local_map = Value::NIL;
        ev.registers = RegisterManager::new();
        ev.bookmarks = BookmarkManager::new();
        ev.abbrevs = AbbrevManager::new();
        ev.autoloads = AutoloadManager::new();
        ev.custom = CustomManager::new();
        ev.rectangle = RectangleState::new();
        ev.interactive = InteractiveRegistry::new();
        ev.input_mode_interrupt = false;
        ev.frames = FrameManager::new();
        ev.modes = ModeRegistry::new();
        ev.threads = ThreadManager::new();
        ev.kmacro = KmacroManager::new();
        ev.command_loop = crate::keyboard::CommandLoop::default();
        ev.input_rx = None;
        #[cfg(unix)]
        {
            ev.wakeup_fd = None;
        }
        ev.redisplay_fn = None;
        ev.display_host = None;
        ev.coding_systems = CodingSystemManager::new();
        ev.face_table = FaceTable::new();
        ev.depth = 0;
        ev.max_depth = 1600;
        ev.gc_pending = false;
        ev.gc_count = 0;
        ev.gc_stress = false;
        ev.temp_roots.clear();
        ev.condition_stack.clear();
        ev.next_resume_id = 1;
        ev.saved_lexenvs.clear();
        ev.named_call_cache.clear();
        ev.source_literal_cache.clear();
        ev.macro_expansion_cache.clear();
        ev.macro_cache_hits = 0;
        ev.macro_cache_misses = 0;
        ev.macro_expand_total_us = 0;
        ev.macro_cache_disabled = false;
        ev.interpreted_closure_filter_fn = None;
        ev.interpreted_closure_trim_cache.clear();
        ev.materialize_public_evaluator_function_cells();
        ev.sync_thread_runtime_bindings();
        ev.sync_current_thread_buffer_state();
        ev
    }

    pub(crate) fn push_condition_frame(&mut self, frame: ConditionFrame) {
        self.condition_stack.push(frame);
    }

    pub(crate) fn pop_condition_frame(&mut self) -> Option<ConditionFrame> {
        self.condition_stack.pop()
    }

    pub(crate) fn truncate_condition_stack(&mut self, len: usize) {
        self.condition_stack.truncate(len);
    }

    pub(crate) fn condition_stack_len(&self) -> usize {
        self.condition_stack.len()
    }

    pub(crate) fn allocate_resume_id(&mut self) -> u64 {
        let resume_id = self.next_resume_id;
        self.next_resume_id += 1;
        resume_id
    }

    pub(crate) fn matching_catch_resume(&self, tag: &Value) -> Option<ResumeTarget> {
        if tag.is_nil() {
            return None;
        }

        self.condition_stack
            .iter()
            .rev()
            .find_map(|frame| match frame {
                ConditionFrame::Catch {
                    tag: catch_tag,
                    resume,
                } if eq_value(catch_tag, tag) => Some(resume.clone()),
                _ => None,
            })
    }

    pub(crate) fn has_active_catch(&self, tag: &Value) -> bool {
        self.matching_catch_resume(tag).is_some()
    }

    pub(crate) fn dispatch_signal_if_needed(
        &mut self,
        sig: SignalData,
    ) -> Result<SignalData, Flow> {
        if sig.search_complete {
            return Ok(sig);
        }
        self.dispatch_signal(sig)
    }

    fn dispatch_signal(&mut self, mut sig: SignalData) -> Result<SignalData, Flow> {
        self.run_signal_hook(&sig)?;
        sig = self.canonicalize_signal_symbol(sig);

        let mut idx = self.condition_stack.len();
        let mut seen_condition_entries = 0usize;

        while let Some(next_idx) = idx.checked_sub(1) {
            idx = next_idx;
            match self.condition_stack[idx].clone() {
                ConditionFrame::Catch { .. } => {}
                ConditionFrame::SkipConditions { remaining } => {
                    let mut to_skip = remaining;
                    while idx > 0 && to_skip > 0 {
                        idx -= 1;
                        if matches!(
                            self.condition_stack[idx],
                            ConditionFrame::ConditionCase { .. }
                                | ConditionFrame::HandlerBind { .. }
                        ) {
                            to_skip -= 1;
                        }
                    }
                }
                ConditionFrame::ConditionCase { conditions, resume } => {
                    seen_condition_entries += 1;
                    if crate::emacs_core::errors::signal_matches_condition_value(
                        &self.obarray,
                        sig.symbol_name(),
                        &conditions,
                    ) {
                        self.maybe_call_debugger_for_signal(&sig, Some(&conditions))?;
                        sig.selected_resume = Some(resume);
                        sig.search_complete = true;
                        return Ok(sig);
                    }
                }
                ConditionFrame::HandlerBind {
                    conditions,
                    handler,
                    mute_span,
                } => {
                    seen_condition_entries += 1;
                    if !crate::emacs_core::errors::signal_matches_condition_value(
                        &self.obarray,
                        sig.symbol_name(),
                        &conditions,
                    ) {
                        continue;
                    }

                    let scope = self.open_gc_scope();
                    for value in &sig.data {
                        self.push_temp_root(*value);
                    }
                    if let Some(raw) = &sig.raw_data {
                        self.push_temp_root(*raw);
                    }

                    self.push_condition_frame(ConditionFrame::SkipConditions {
                        remaining: seen_condition_entries + mute_span,
                    });

                    let handler_result = self.apply(handler, vec![make_signal_binding_value(&sig)]);

                    match handler_result {
                        Ok(_) => {
                            self.pop_condition_frame();
                            scope.close(self);
                            continue;
                        }
                        Err(Flow::Signal(next_sig)) => {
                            let dispatched = self.dispatch_signal_if_needed(next_sig);
                            self.pop_condition_frame();
                            scope.close(self);
                            return dispatched;
                        }
                        Err(flow @ Flow::Throw { .. }) => {
                            self.pop_condition_frame();
                            scope.close(self);
                            return Err(flow);
                        }
                    }
                }
            }
        }

        self.maybe_call_debugger_for_signal(&sig, None)?;
        sig.search_complete = true;
        sig.selected_resume = None;
        Ok(sig)
    }

    fn run_signal_hook(&mut self, sig: &SignalData) -> Result<(), Flow> {
        if sig.suppress_signal_hook {
            return Ok(());
        }

        let hook = self
            .obarray
            .symbol_value("signal-hook-function")
            .copied()
            .unwrap_or(Value::NIL);
        if hook.is_nil() {
            return Ok(());
        }

        self.apply(
            hook,
            vec![
                Value::from_sym_id(sig.symbol),
                signal_hook_payload_value(sig),
            ],
        )
        .map(|_| ())
    }

    fn canonicalize_signal_symbol(&self, sig: SignalData) -> SignalData {
        let sym_name = sig.symbol_name();
        if sym_name == "error" || sym_name == "quit" {
            return sig;
        }
        if self
            .obarray
            .get_property(sym_name, "error-conditions")
            .is_some()
        {
            return sig;
        }

        SignalData {
            symbol: intern("error"),
            data: vec![
                Value::string("Invalid error symbol"),
                Value::from_sym_id(sig.symbol),
            ],
            raw_data: None,
            suppress_signal_hook: sig.suppress_signal_hook,
            selected_resume: None,
            search_complete: false,
        }
    }

    fn maybe_call_debugger_for_signal(
        &mut self,
        sig: &SignalData,
        matched_clause: Option<&Value>,
    ) -> Result<(), Flow> {
        if self
            .obarray
            .symbol_value("inhibit-debugger")
            .is_some_and(|value| !value.is_nil())
        {
            return Ok(());
        }

        let debug_on_signal = self
            .obarray
            .symbol_value("debug-on-signal")
            .is_some_and(|value| !value.is_nil());
        let should_consider_debugger = debug_on_signal
            || matched_clause.is_none()
            || matched_clause.is_some_and(condition_value_contains_debug);
        if !should_consider_debugger {
            return Ok(());
        }

        let conditions = self.signal_conditions_value(sig);
        let debug_setting = if crate::emacs_core::errors::signal_matches_hierarchical(
            &self.obarray,
            sig.symbol_name(),
            "quit",
        ) {
            self.obarray
                .symbol_value("debug-on-quit")
                .copied()
                .unwrap_or(Value::NIL)
        } else {
            self.obarray
                .symbol_value("debug-on-error")
                .copied()
                .unwrap_or(Value::NIL)
        };
        if !wants_debugger(&debug_setting, &conditions) {
            return Ok(());
        }
        if self.skip_debugger(sig, &conditions)? {
            return Ok(());
        }

        self.call_debugger_for_signal(sig)
    }

    fn signal_conditions_value(&self, sig: &SignalData) -> Value {
        self.obarray
            .get_property(sig.symbol_name(), "error-conditions")
            .copied()
            .unwrap_or_else(|| Value::list(vec![Value::from_sym_id(sig.symbol)]))
    }

    fn skip_debugger(&mut self, sig: &SignalData, conditions: &Value) -> Result<bool, Flow> {
        let ignored = self
            .obarray
            .symbol_value("debug-ignored-errors")
            .copied()
            .unwrap_or(Value::NIL);
        let Some(entries) = list_to_vec(&ignored) else {
            return Ok(false);
        };
        if entries.is_empty() {
            return Ok(false);
        }

        let mut error_message = None;
        let error_data = make_signal_binding_value(sig);
        let signal_conditions = list_to_vec(conditions).unwrap_or_else(|| vec![*conditions]);

        for entry in entries {
            if entry.as_str().is_some() {
                let message = if let Some(message) = error_message {
                    message
                } else {
                    let rendered = crate::emacs_core::errors::builtin_error_message_string(
                        self,
                        vec![error_data],
                    )?;
                    error_message = Some(rendered);
                    rendered
                };

                if builtins::search::builtin_string_match_p_with_case_fold(
                    false,
                    &[entry, message],
                )?
                .as_fixnum()
                .is_some()
                {
                    return Ok(true);
                }
                continue;
            }

            if signal_conditions.iter().any(|item| *item == entry) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn call_debugger_for_signal(&mut self, sig: &SignalData) -> Result<(), Flow> {
        let debugger = self
            .obarray
            .symbol_value("debugger")
            .copied()
            .unwrap_or(Value::NIL);
        let specpdl_count = self.specpdl.len();
        self.specbind(intern("debugger-may-continue"), Value::T);
        self.specbind(intern("inhibit-debugger"), Value::T);
        let result = self.apply(
            debugger,
            vec![Value::symbol("error"), make_signal_binding_value(sig)],
        );
        self.unbind_to(specpdl_count);
        result.map(|_| ())
    }

    fn new_inner(reset_thread_locals: bool) -> Self {
        // Create the interner and heap, set thread-locals so that Value
        // constructors (symbol, keyword, cons, list, etc.) work during init.
        let mut interner = Box::new(StringInterner::new());
        set_current_interner(&mut interner);
        let mut tagged_heap = Box::new(crate::tagged::gc::TaggedHeap::new());
        crate::tagged::gc::set_tagged_heap(&mut tagged_heap);

        // Clear any caches that hold heap-allocated Values (tagged pointers) from a
        // previous heap. Critical for test isolation when multiple Contexts
        // are created sequentially on the same thread.
        if reset_thread_locals {
            super::syntax::reset_syntax_thread_locals();
            super::casetab::reset_casetab_thread_locals();
            super::category::reset_category_thread_locals();
            // Only reset the terminal handle (stale reference), not
            // the full terminal runtime/params which may be pre-
            // configured by tests before Context creation.
            super::terminal::pure::reset_terminal_handle();
            super::value::reset_string_text_properties();
            super::ccl::reset_ccl_registry();
            super::dispnew::pure::reset_dispnew_thread_locals();
            super::font::clear_font_cache_state();
            super::builtins::reset_builtins_thread_locals();
            super::charset::reset_charset_registry();
            super::timefns::reset_timefns_thread_locals();
        }

        let mut obarray = Obarray::new();
        // Builtin names are interned by defsubr() during init_builtins(),
        // which runs after Context construction.
        let default_directory = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .map(|mut s| {
                if !s.ends_with('/') {
                    s.push('/');
                }
                s
            })
            .unwrap_or_else(|| "./".to_string());
        // Create all keymaps as Emacs-compatible cons-list values
        let completion_in_region_mode_map = make_sparse_list_keymap();
        let completion_list_mode_map = make_sparse_list_keymap();
        let minibuffer_local_map = make_sparse_list_keymap();
        // Keep only the base minibuffer map here. GNU Lisp defines
        // `read-expression-map` / `read--expression-map` itself in simple.el via
        // `defvar-keymap`; prebinding them here causes those definitions to be
        // skipped, which leaves RET/C-j handling diverged from GNU Emacs.
        // Standard keymaps required by loadup.el files (normally created by C code)
        // `global-map`, `esc-map`, `ctl-x-map`, and `help-map` are defined in GNU Lisp,
        // so keep them unbound here and let the Lisp `defvar` / `defvar-keymap`
        // initializers run.  Prebinding them here causes GNU definitions like
        // help.el's `defvar-keymap help-map ...` to skip installing their real
        // bindings.
        let special_event_map = make_sparse_list_keymap();
        let mode_line_window_dedicated_keymap = make_sparse_list_keymap();
        let indent_rigidly_map = make_sparse_list_keymap();
        let text_mode_map = make_sparse_list_keymap();
        let image_slice_map = make_sparse_list_keymap();
        let tool_bar_map = make_sparse_list_keymap();
        let key_translation_map = make_sparse_list_keymap();
        let function_key_map = make_sparse_list_keymap();
        let input_decode_map = make_sparse_list_keymap();
        let local_function_key_map = make_sparse_list_keymap();
        // GNU Emacs: local-function-key-map inherits from function-key-map
        // (keyboard.c:13097). Without this, bindings in function-key-map
        // (like [backspace] → [?\C-?]) are not found during key translation.
        list_keymap_set_parent(local_function_key_map, function_key_map);
        // GNU keyboard.c seeds special-event-map with delete-frame and focus
        // handlers at C bootstrap time and leaves hook semantics to frame.el.
        list_keymap_define(
            special_event_map,
            Value::symbol("delete-frame"),
            Value::symbol("handle-delete-frame"),
        );
        list_keymap_define(
            special_event_map,
            Value::symbol("focus-in"),
            Value::symbol("handle-focus-in"),
        );
        list_keymap_define(
            special_event_map,
            Value::symbol("focus-out"),
            Value::symbol("handle-focus-out"),
        );

        let standard_syntax_table = super::syntax::builtin_standard_syntax_table(Vec::new())
            .expect("startup seeding requires standard syntax table");
        let standard_category_table = super::category::ensure_standard_category_table_object()
            .expect("startup seeding requires standard category table");

        // Set up standard global variables
        // Match GNU Emacs: MOST_POSITIVE_FIXNUM = EMACS_INT_MAX >> INTTYPEBITS (>> 2)
        // These are SYMBOL_NOWRITE constants in GNU Emacs (cannot be setq'd).
        obarray.set_symbol_value("most-positive-fixnum", Value::fixnum(i64::MAX >> 2));
        obarray.set_constant("most-positive-fixnum");
        obarray.set_symbol_value("most-negative-fixnum", Value::fixnum(-(i64::MAX >> 2) - 1));
        obarray.set_constant("most-negative-fixnum");
        // Mathematical constants (defconst in float-sup.el)
        obarray.set_symbol_value("float-e", Value::make_float(std::f64::consts::E));
        obarray.set_symbol_value("float-pi", Value::make_float(std::f64::consts::PI));
        obarray.set_symbol_value("pi", Value::make_float(std::f64::consts::PI));
        obarray.set_symbol_value("emacs-version", Value::string("31.0.50"));
        obarray.set_symbol_value("emacs-major-version", Value::fixnum(31));
        obarray.set_symbol_value("emacs-minor-version", Value::fixnum(0));
        obarray.set_symbol_value("emacs-build-number", Value::fixnum(1));
        obarray.set_symbol_value("system-type", Value::symbol("gnu/linux"));
        obarray.set_symbol_value(
            "default-directory",
            Value::string(default_directory.clone()),
        );
        obarray.set_symbol_value(
            "command-line-default-directory",
            Value::string(default_directory),
        );
        let obarray_object = Value::vector(vec![Value::NIL]);
        obarray.set_symbol_value("obarray", obarray_object);
        obarray.set_symbol_value("neovm--obarray-object", obarray_object);
        obarray.make_special("obarray");
        obarray.set_symbol_value("standard-input", Value::T);
        obarray.make_special("standard-input");
        obarray.set_symbol_value(
            "command-line-args",
            Value::list(vec![
                Value::string("neovm-worker"),
                Value::string("--batch"),
            ]),
        );
        obarray.set_symbol_value("command-line-args-left", Value::NIL);
        obarray.set_symbol_value("command-line-functions", Value::NIL);
        obarray.set_symbol_value("command-line-processed", Value::T);
        obarray.set_symbol_value("command-switch-alist", Value::NIL);
        // GNU emacs.c: set from argv[0]. NeoVM uses current exe path.
        let exe_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok());
        let invocation_name = exe_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "neomacs".to_string());
        let invocation_directory = exe_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|d| format!("{}/", d.to_string_lossy()))
            .unwrap_or_else(|| "./".to_string());
        obarray.set_symbol_value("invocation-name", Value::string(invocation_name));
        obarray.set_symbol_value("invocation-directory", Value::string(invocation_directory));
        obarray.set_symbol_value("installation-directory", Value::NIL);
        obarray.set_symbol_value("configure-info-directory", Value::NIL);
        // GNU keyboard.c: internal--top-level-message for command loop entry
        obarray.set_symbol_value(
            "internal--top-level-message",
            Value::string("Back to top level"),
        );
        obarray.set_symbol_value("charset-map-path", Value::NIL);
        obarray.set_symbol_value("doc-directory", Value::NIL);
        // warnings.el defcustom — needed before warnings.el loads
        obarray.set_symbol_value("warning-minimum-log-level", Value::keyword(":warning"));
        obarray.set_symbol_value("warning-minimum-level", Value::keyword(":warning"));
        obarray.set_symbol_value("process-environment", Value::NIL);
        obarray.set_symbol_value("initial-environment", Value::NIL);
        obarray.set_symbol_value("path-separator", Value::string(":"));
        obarray.set_symbol_value("shared-game-score-directory", Value::NIL);
        obarray.set_symbol_value("system-messages-locale", Value::NIL);
        obarray.set_symbol_value("system-time-locale", Value::NIL);
        obarray.set_symbol_value("before-init-time", Value::NIL);
        obarray.set_symbol_value("after-init-time", Value::NIL);
        obarray.set_symbol_value(
            "system-configuration",
            super::builtins_extra::system_configuration_value(),
        );
        obarray.set_symbol_value(
            "system-configuration-options",
            super::builtins_extra::system_configuration_options_value(),
        );
        obarray.set_symbol_value(
            "system-configuration-features",
            super::builtins_extra::system_configuration_features_value(),
        );
        obarray.set_symbol_value("system-name", Value::string("localhost"));
        obarray.set_symbol_value("user-full-name", Value::string("unknown"));
        obarray.set_symbol_value("user-login-name", Value::string("unknown"));
        obarray.set_symbol_value("user-real-login-name", Value::string("unknown"));
        obarray.set_symbol_value(
            "operating-system-release",
            super::builtins_extra::operating_system_release_value(),
        );
        obarray.set_symbol_value("delayed-warnings-list", Value::NIL);
        obarray.set_symbol_value(
            "command-line-ns-option-alist",
            Value::list(vec![Value::list(vec![
                Value::string("-NSOpen"),
                Value::fixnum(1),
                Value::symbol("ns-handle-nxopen"),
            ])]),
        );
        obarray.set_symbol_value(
            "command-line-x-option-alist",
            Value::list(vec![Value::list(vec![
                Value::string("-display"),
                Value::fixnum(1),
                Value::symbol("x-handle-display"),
            ])]),
        );
        obarray.set_symbol_value("load-path", Value::NIL);
        obarray.make_special("load-path");
        obarray.set_symbol_value("load-history", Value::NIL);
        obarray.set_symbol_value(
            "fontset-alias-alist",
            super::builtins::symbols::fontset_alias_alist_startup_value(),
        );
        // GNU Emacs: load-suffixes defaults to (".elc" ".el").
        // NeoVM matches this — prefer compiled bytecode, fall back to source.
        obarray.set_symbol_value(
            "load-suffixes",
            Value::list(vec![Value::string(".elc"), Value::string(".el")]),
        );
        obarray.make_special("load-suffixes");
        obarray.set_symbol_value("module-file-suffix", Value::NIL);
        obarray.make_special("module-file-suffix");
        obarray.set_symbol_value(
            "dynamic-library-suffixes",
            Value::list(vec![Value::string(std::env::consts::DLL_SUFFIX)]),
        );
        obarray.make_special("dynamic-library-suffixes");
        // load-file-rep-suffixes: suffixes for alternate representations of
        // the same file (e.g., compressed ".gz").  Default is just ("").
        obarray.set_symbol_value(
            "load-file-rep-suffixes",
            Value::list(vec![Value::string("")]),
        );
        obarray.make_special("load-file-rep-suffixes");
        // file-coding-system-alist: needed by jka-cmpr-hook.el and others.
        obarray.set_symbol_value("file-coding-system-alist", Value::NIL);
        obarray.set_symbol_value("features", Value::NIL);
        obarray.set_symbol_value("lexical-binding", Value::NIL);
        obarray.set_symbol_value("load-prefer-newer", Value::NIL);
        obarray.set_symbol_value("load-file-name", Value::NIL);
        obarray.make_special("load-file-name");
        obarray.set_symbol_value("noninteractive", Value::T);
        obarray.set_symbol_value("inhibit-quit", Value::NIL);
        // GNU Emacs print.c: all print-* variables are DEFVAR_BOOL or
        // DEFVAR_LISP, making them dynamically scoped (special).
        // This is essential so `(let ((print-escape-newlines t)) ...)`
        // affects the C print code via dynamic binding.
        for name in [
            "print-length",
            "print-level",
            "print-circle",
            "print-quoted",
            "print-escape-newlines",
            "print-escape-control-characters",
            "print-escape-nonascii",
            "print-escape-multibyte",
            "print-gensym",
            "print-continuous-numbering",
            "print-number-table",
            "print-charset-text-property",
            "print-integers-as-characters",
            "print-unreadable-function",
        ] {
            obarray.set_symbol_value(name, Value::NIL);
            obarray.make_special(name);
        }
        obarray.set_symbol_value("print-quoted", Value::T);
        obarray.set_symbol_value("text-quoting-style", Value::NIL);
        // GNU DEFVAR_LISP variables needed by loadup.el and early .el files.
        obarray.set_symbol_value("char-code-property-alist", Value::NIL);
        obarray.set_symbol_value("redisplay--inhibit-bidi", Value::NIL);
        obarray.set_symbol_value("resize-mini-windows", Value::NIL);

        // GNU C variables checked by cus-start.el during bootstrap.
        // 178 DEFVAR_LISP/DEFVAR_INT/DEFVAR_BOOL variables extracted from
        // GNU Emacs -Q. Default values match GNU's init_*() functions.
        for name in [
            "alter-fullscreen-frames",
            "auto-save-no-message",
            "auto-save-visited-file-name",
            "blink-cursor-alist",
            "composition-break-at-point",
            "debug-on-quit",
            "debugger-stack-frame-as-list",
            "default-frame-alist",
            "delete-by-moving-to-trash",
            "display-fill-column-indicator",
            "display-fill-column-indicator-character",
            "display-line-numbers",
            "display-line-numbers-widen",
            "display-line-numbers-width",
            "display-raw-bytes-as-hex",
            "echo-keystrokes-help",
            "enable-character-translation",
            "enable-recursive-minibuffers",
            "fast-but-imprecise-scrolling",
            "focus-follows-mouse",
            "font-use-system-font",
            "frame-resize-pixelwise",
            "garbage-collection-messages",
            "highlight-nonselected-windows",
            "history-delete-duplicates",
            "inhibit-eol-conversion",
            "inverse-video",
            "kill-buffer-delete-auto-save-files",
            "line-number-display-limit",
            "make-pointer-invisible",
            "menu-bar-mode",
            "minibuffer-auto-raise",
            "mode-line-compact",
            "mouse-autoselect-window",
            "mouse-prefer-closest-glyph",
            "no-redraw-on-reenter",
            "parse-sexp-ignore-comments",
            "read-buffer-completion-ignore-case",
            "record-all-keys",
            "resize-mini-frames",
            "ring-bell-function",
            "scalable-fonts-allowed",
            "scroll-preserve-screen-position",
            "show-trailing-whitespace",
            "tab-bar-mode",
            "tab-bar-position",
            "temp-buffer-show-function",
            "tool-bar-mode",
            "tool-bar-style",
            "tooltip-reuse-hidden-frame",
            "treesit-extra-load-path",
            "undo-outer-limit",
            "unibyte-display-via-language-environment",
            "use-short-answers",
            "visible-bell",
            "window-combination-resize",
            "window-resize-pixelwise",
            "word-wrap-by-category",
            "words-include-escapes",
            "x-dnd-disable-motif-drag",
            "x-gtk-show-hidden-files",
            "x-gtk-use-native-input",
            "x-gtk-use-old-file-dialog",
            "x-stretch-cursor",
            "x-underline-at-descent-line",
            "x-use-underline-position-properties",
        ] {
            obarray.set_symbol_value(name, Value::NIL);
        }
        for name in [
            "auto-hscroll-mode",
            "create-lockfiles",
            "delete-auto-save-files",
            "delete-exited-processes",
            "display-fill-column-indicator-column",
            "display-hourglass",
            "display-line-numbers-current-absolute",
            "make-cursor-line-fully-visible",
            "menu-prompting",
            "mode-line-in-non-selected-windows",
            "mouse-highlight",
            "open-paren-in-column-0-is-defun-start",
            "overflow-newline-into-fringe",
            "read-minibuffer-restore-windows",
            "scroll-bar-adjust-thumb-portion",
            "select-active-regions",
            "translate-upper-case-key-bindings",
            "use-dialog-box",
            "use-file-dialog",
            "use-system-tooltips",
            "visible-cursor",
            "x-gtk-file-dialog-help-text",
            "x-select-enable-clipboard-manager",
        ] {
            obarray.set_symbol_value(name, Value::T);
        }
        obarray.set_symbol_value("auto-save-interval", Value::fixnum(300));
        obarray.set_symbol_value("auto-save-timeout", Value::fixnum(30));
        obarray.set_symbol_value("display-line-numbers-major-tick", Value::fixnum(0));
        obarray.set_symbol_value("display-line-numbers-minor-tick", Value::fixnum(0));
        obarray.set_symbol_value("double-click-fuzz", Value::fixnum(3));
        obarray.set_symbol_value("double-click-time", Value::fixnum(500));
        obarray.set_symbol_value("echo-keystrokes", Value::fixnum(1));
        obarray.set_symbol_value("gc-cons-threshold", Value::fixnum(800000));
        obarray.set_symbol_value("help-char", Value::fixnum(8));
        obarray.set_symbol_value("hourglass-delay", Value::fixnum(1));
        obarray.set_symbol_value("hscroll-margin", Value::fixnum(5));
        obarray.set_symbol_value("hscroll-step", Value::fixnum(0));
        obarray.set_symbol_value("line-number-display-limit-width", Value::fixnum(200));
        obarray.set_symbol_value("maximum-scroll-margin", Value::fixnum(25));
        obarray.set_symbol_value("message-log-max", Value::fixnum(1000));
        obarray.set_symbol_value("meta-prefix-char", Value::fixnum(27));
        obarray.set_symbol_value("next-screen-context-lines", Value::fixnum(2));
        obarray.set_symbol_value("overline-margin", Value::fixnum(2));
        obarray.set_symbol_value("polling-period", Value::fixnum(2));
        obarray.set_symbol_value("process-error-pause-time", Value::fixnum(1));
        obarray.set_symbol_value("scroll-conservatively", Value::fixnum(0));
        obarray.set_symbol_value("scroll-margin", Value::fixnum(0));
        obarray.set_symbol_value("scroll-step", Value::fixnum(0));
        obarray.set_symbol_value("tool-bar-max-label-size", Value::fixnum(10));
        obarray.set_symbol_value("truncate-partial-width-windows", Value::fixnum(50));
        obarray.set_symbol_value("underline-minimum-offset", Value::fixnum(1));
        obarray.set_symbol_value("undo-limit", Value::fixnum(160000));
        obarray.set_symbol_value("undo-strong-limit", Value::fixnum(240000));
        obarray.set_symbol_value("eol-mnemonic-dos", Value::string("\\"));
        obarray.set_symbol_value("eol-mnemonic-mac", Value::string("/"));
        obarray.set_symbol_value("eol-mnemonic-undecided", Value::string(":"));
        obarray.set_symbol_value("eol-mnemonic-unix", Value::string(":"));
        obarray.set_symbol_value(
            "report-emacs-bug-address",
            Value::string("bug-gnu-emacs@gnu.org"),
        );
        obarray.set_symbol_value("yes-or-no-prompt", Value::string("(yes or no) "));
        // Float-valued C variables
        obarray.set_symbol_value("gc-cons-percentage", Value::make_float(0.1));
        obarray.set_symbol_value("max-mini-window-height", Value::make_float(0.25));
        obarray.set_symbol_value("image-scaling-factor", Value::make_float(1.0));
        // Display engine C variables (xdisp.c)
        obarray.set_symbol_value("global-mode-string", Value::NIL);
        // File loading C variables (lread.c)
        obarray.set_symbol_value("load-in-progress", Value::NIL);
        // Process/daemon C variables (process.c)
        obarray.set_symbol_value("internal--daemon-sockname", Value::NIL);
        // Byte compiler variables (bytecomp.el defcustom, but referenced
        // at runtime by legacy packages like evil-escape via ad-add-advice)
        obarray.set_symbol_value("byte-compile-warnings", Value::T);
        // Other missing C variables cus-start.el checks
        obarray.set_symbol_value("history-length", Value::fixnum(100));
        obarray.set_symbol_value("minibuffer-follows-selected-frame", Value::T);
        obarray.set_symbol_value("recenter-redisplay", Value::symbol("tty"));
        obarray.set_symbol_value("iconify-child-frame", Value::symbol("iconify-top-level"));
        obarray.set_symbol_value("frame-inhibit-implied-resize", Value::NIL);
        obarray.set_symbol_value("mark-even-if-inactive", Value::T);
        obarray.set_symbol_value("read-buffer-function", Value::NIL);
        obarray.set_symbol_value("minibuffer-prompt-properties", Value::NIL);
        obarray.set_symbol_value("help-event-list", Value::NIL);
        obarray.set_symbol_value("debug-ignored-errors", Value::NIL);
        obarray.set_symbol_value("debug-on-event", Value::NIL);
        obarray.set_symbol_value("debug-on-signal", Value::NIL);
        // Remaining cus-start.el variables (general + platform stubs)
        for name in [
            "imagemagick-render-type",
            "window-combination-limit",
            "void-text-area-pointer",
            "x-bitmap-file-path",
            "x-gtk-use-system-tooltips",
            "x-scroll-event-delta-factor",
            "x-auto-preserve-selections",
            "xwidget-internal",
            "temporary-file-directory",
            "vertical-centering-font-regexp",
            "ns-control-modifier",
            "ns-right-control-modifier",
            "ns-command-modifier",
            "ns-right-command-modifier",
            "ns-alternate-modifier",
            "ns-right-alternate-modifier",
            "ns-function-modifier",
            "ns-antialias-text",
            "ns-auto-hide-menu-bar",
            "ns-confirm-quit",
            "ns-use-native-fullscreen",
            "ns-use-fullscreen-animation",
            "ns-use-srgb-colorspace",
            "ns-scroll-event-delta-factor",
            "ns-click-through",
            "w32-follow-system-dark-mode",
            "dos-display-scancodes",
            "dos-hyper-key",
            "dos-super-key",
            "dos-keypad-mode",
            "dos-unsupported-char-glyph",
            "haiku-debug-on-fatal-error",
            "haiku-use-system-tooltips",
            "xwidget-webkit-disable-javascript",
        ] {
            obarray.set_symbol_value(name, Value::NIL);
        }

        // GNU DEFVAR_LISP variables from lread.c that must be bound to nil
        // before any Elisp runs (code may test `boundp` or read them directly).
        //
        // Keep GNU's exception for `values`: `lread.c` defines it via
        // `DEFVAR_LISP` and then explicitly clears the declared-special bit,
        // so it remains an ordinary variable even under lexical binding.
        obarray.set_symbol_value("values", Value::NIL);
        obarray.set_symbol_value("eval-buffer-list", Value::NIL);
        obarray.make_special("eval-buffer-list");
        obarray.set_symbol_value("lread--unescaped-character-literals", Value::NIL);
        obarray.make_special("lread--unescaped-character-literals");
        obarray.set_symbol_value("load-read-function", Value::symbol("read"));
        obarray.make_special("load-read-function");
        obarray.set_symbol_value("load-source-file-function", Value::NIL);
        obarray.make_special("load-source-file-function");
        obarray.set_symbol_value("load-true-file-name", Value::NIL);
        obarray.make_special("load-true-file-name");
        obarray.set_symbol_value("user-init-file", Value::NIL);
        obarray.make_special("user-init-file");
        obarray.set_symbol_value("source-directory", Value::NIL);
        obarray.make_special("source-directory");
        obarray.set_symbol_value("after-load-alist", Value::NIL);
        obarray.make_special("after-load-alist");
        obarray.set_symbol_value("load-history", Value::NIL);
        obarray.make_special("load-history");
        obarray.set_symbol_value("current-load-list", Value::NIL);
        obarray.make_special("current-load-list");
        obarray.set_symbol_value("preloaded-file-list", Value::NIL);
        obarray.make_special("preloaded-file-list");
        obarray.set_symbol_value("byte-boolean-vars", Value::NIL);
        obarray.make_special("byte-boolean-vars");
        obarray.set_symbol_value(
            "bytecomp-version-regexp",
            Value::string(r#"^;;;.\(in Emacs version\|bytecomp version FSF\)"#),
        );
        obarray.make_special("bytecomp-version-regexp");
        obarray.set_symbol_value("load-path-filter-function", Value::NIL);
        obarray.make_special("load-path-filter-function");
        obarray.set_symbol_value("internal--get-default-lexical-binding-function", Value::NIL);
        obarray.make_special("internal--get-default-lexical-binding-function");
        obarray.set_symbol_value("read-symbol-shorthands", Value::NIL);
        obarray.make_special("read-symbol-shorthands");
        obarray.set_symbol_value("macroexp--dynvars", Value::NIL);
        obarray.make_special("macroexp--dynvars");
        // GNU DEFVAR_LISP variables from eval.c / keyboard.c.
        let quit_flag_symbol = intern("quit-flag");
        obarray.set_symbol_value_id(quit_flag_symbol, Value::NIL);
        obarray.make_special_id(quit_flag_symbol);
        let inhibit_quit_symbol = intern("inhibit-quit");
        obarray.set_symbol_value_id(inhibit_quit_symbol, Value::NIL);
        obarray.make_special_id(inhibit_quit_symbol);
        let throw_on_input_symbol = intern("throw-on-input");
        obarray.set_symbol_value_id(throw_on_input_symbol, Value::NIL);
        obarray.make_special_id(throw_on_input_symbol);
        let kill_emacs_symbol = intern("kill-emacs");
        obarray.set_symbol_value("inhibit-debugger", Value::NIL);
        obarray.make_special("inhibit-debugger");
        obarray.set_symbol_value("debug-on-error", Value::NIL);
        obarray.make_special("debug-on-error");
        obarray.set_symbol_value("debug-on-quit", Value::NIL);
        obarray.make_special("debug-on-quit");
        obarray.set_symbol_value("debug-on-signal", Value::NIL);
        obarray.make_special("debug-on-signal");
        obarray.set_symbol_value("debug-ignored-errors", Value::NIL);
        obarray.make_special("debug-ignored-errors");
        obarray.set_symbol_value("debugger-may-continue", Value::NIL);
        obarray.make_special("debugger-may-continue");
        obarray.set_symbol_value("internal-when-entered-debugger", Value::fixnum(-1));
        obarray.make_special("internal-when-entered-debugger");
        obarray.set_symbol_value("signal-hook-function", Value::NIL);
        obarray.make_special("signal-hook-function");
        // GNU `eval.c` defines `internal-interpreter-environment` and then
        // immediately `Funintern`s that symbol, so Lisp-visible lookup sees a
        // separate ordinary symbol while the evaluator keeps a hidden special
        // variable for its own lexical-environment bookkeeping.
        obarray.intern("internal-interpreter-environment");
        let internal_interpreter_environment_symbol =
            intern_uninterned("internal-interpreter-environment");
        obarray.set_symbol_value_id(internal_interpreter_environment_symbol, Value::NIL);
        obarray.make_special_id(internal_interpreter_environment_symbol);
        obarray.set_symbol_value("internal-make-interpreted-closure-function", Value::NIL);
        obarray.make_special("internal-make-interpreted-closure-function");
        // GNU seeds `debugger` from eval.c before Lisp startup.
        // `eval-expression` relies on it.
        obarray.set_symbol_value("debugger", Value::symbol("debug-early"));
        obarray.make_special("debugger");
        obarray.set_symbol_value("standard-output", Value::T);
        // GNU DEFVAR_INT from dispnew.c — used by bytecomp.el
        obarray.set_symbol_value("baud-rate", Value::fixnum(38400));
        obarray.set_symbol_value("search-slow-speed", Value::fixnum(1200));
        // GNU startup.el sets these based on --debug-init
        obarray.set_symbol_value("init-file-debug", Value::NIL);
        // GNU callproc.c: exec-path is built from PATH env var.
        // exec-directory is the directory containing helper programs.
        let exec_path: Vec<Value> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|s| Value::string(s.to_string()))
            .collect();
        obarray.set_symbol_value("exec-path", Value::list(exec_path));
        obarray.set_symbol_value(
            "exec-directory",
            Value::string(
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
                    .unwrap_or_else(|| "/usr/bin/".to_string()),
            ),
        );
        obarray.set_symbol_value("exec-suffixes", Value::list(vec![Value::string("")]));
        obarray.set_symbol_value("buffer-read-only", Value::NIL);
        obarray.set_symbol_value("left-margin-width", Value::NIL);
        obarray.set_symbol_value("right-margin-width", Value::NIL);
        obarray.set_symbol_value("left-fringe-width", Value::NIL);
        obarray.set_symbol_value("right-fringe-width", Value::NIL);
        obarray.set_symbol_value("fringes-outside-margins", Value::NIL);
        obarray.set_symbol_value("scroll-bar-width", Value::NIL);
        obarray.set_symbol_value("scroll-bar-height", Value::NIL);
        obarray.set_symbol_value("vertical-scroll-bar", Value::T);
        obarray.set_symbol_value("horizontal-scroll-bar", Value::T);
        obarray.set_symbol_value("kill-ring", Value::NIL);
        obarray.set_symbol_value("kill-ring-yank-pointer", Value::NIL);
        obarray.set_symbol_value("last-command", Value::NIL);
        obarray.set_symbol_value("current-fill-column--has-warned", Value::NIL);
        obarray.set_symbol_value("current-input-method", Value::NIL);
        obarray.set_symbol_value("current-input-method-title", Value::NIL);
        obarray.set_symbol_value("current-iso639-language", Value::NIL);
        obarray.set_symbol_value("current-key-remap-sequence", Value::NIL);
        obarray.set_symbol_value("current-language-environment", Value::string("UTF-8"));
        obarray.set_symbol_value(
            "current-load-list",
            Value::list(vec![
                Value::symbol("comp--no-native-compile"),
                Value::cons(
                    Value::symbol("defun"),
                    Value::symbol("load--fixup-all-elns"),
                ),
                Value::symbol("load--eln-dest-dir"),
                Value::symbol("load--bin-dest-dir"),
            ]),
        );
        obarray.set_symbol_value("current-locale-environment", Value::string("C.UTF-8"));
        obarray.set_symbol_value("current-minibuffer-command", Value::NIL);
        obarray.set_symbol_value("current-time-list", Value::T);
        obarray.set_symbol_value("current-transient-input-method", Value::NIL);
        obarray.set_symbol_value("real-last-command", Value::NIL);
        obarray.set_symbol_value("last-repeatable-command", Value::NIL);
        obarray.set_symbol_value("this-original-command", Value::NIL);
        obarray.set_symbol_value("prefix-arg", Value::NIL);
        obarray.set_symbol_value("defining-kbd-macro", Value::NIL);
        obarray.set_symbol_value("executing-kbd-macro", Value::NIL);
        obarray.set_symbol_value("executing-kbd-macro-index", Value::fixnum(0));
        obarray.set_symbol_value("kbd-macro-termination-hook", Value::NIL);
        obarray.set_symbol_value("command-history", Value::NIL);
        obarray.set_symbol_value("extended-command-history", Value::NIL);
        obarray.set_symbol_value("completion-ignore-case", Value::NIL);
        obarray.set_symbol_value("read-buffer-completion-ignore-case", Value::NIL);
        obarray.set_symbol_value("read-file-name-completion-ignore-case", Value::NIL);
        obarray.set_symbol_value("completion-regexp-list", Value::NIL);
        obarray.set_symbol_value("completion--all-sorted-completions-location", Value::NIL);
        obarray.set_symbol_value("completion--capf-misbehave-funs", Value::NIL);
        obarray.set_symbol_value("completion--capf-safe-funs", Value::NIL);
        obarray.set_symbol_value(
            "completion--embedded-envvar-re",
            Value::string(
                "\\(?:^\\|[^$]\\(?:\\$\\$\\)*\\)\\$\\([[:alnum:]_]*\\|{\\([^}]*\\)\\)\\'",
            ),
        );
        obarray.set_symbol_value("completion--flex-score-last-md", Value::NIL);
        obarray.set_symbol_value("completion-all-sorted-completions", Value::NIL);
        obarray.set_symbol_value(
            "completion--cycling-threshold-type",
            Value::list(vec![Value::symbol("choice")]),
        );
        obarray.set_symbol_value(
            "completion--styles-type",
            Value::list(vec![Value::symbol("repeat")]),
        );
        obarray.set_symbol_value(
            "completion-at-point-functions",
            Value::list(vec![Value::symbol("tags-completion-at-point-function")]),
        );
        obarray.set_symbol_value(
            "completion-setup-hook",
            Value::list(vec![Value::symbol("completion-setup-function")]),
        );
        obarray.set_symbol_value(
            "completion-in-region-mode-map",
            completion_in_region_mode_map,
        );
        obarray.set_symbol_value("completion-list-mode-map", completion_list_mode_map);
        obarray.set_symbol_value("completion-list-mode-syntax-table", standard_syntax_table);
        obarray.set_symbol_value(
            "completion-list-mode-abbrev-table",
            Value::symbol("completion-list-mode-abbrev-table"),
        );
        obarray.set_symbol_value("completion-list-mode-hook", Value::NIL);
        obarray.set_symbol_value(
            "completion-ignored-extensions",
            Value::list(vec![
                Value::string(".o"),
                Value::string("~"),
                Value::string(".elc"),
            ]),
        );
        obarray.set_symbol_value(
            "completion-styles",
            Value::list(vec![
                Value::symbol("basic"),
                Value::symbol("partial-completion"),
                Value::symbol("emacs22"),
            ]),
        );
        obarray.set_symbol_value(
            "completion-category-defaults",
            Value::list(vec![
                Value::list(vec![
                    Value::symbol("buffer"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("unicode-name"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("project-file"),
                    Value::list(vec![Value::symbol("styles"), Value::symbol("substring")]),
                ]),
                Value::list(vec![
                    Value::symbol("xref-location"),
                    Value::list(vec![Value::symbol("styles"), Value::symbol("substring")]),
                ]),
                Value::list(vec![
                    Value::symbol("info-menu"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("symbol-help"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("shorthand"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("calendar-month"),
                    Value::cons(
                        Value::symbol("display-sort-function"),
                        Value::symbol("identity"),
                    ),
                ]),
            ]),
        );
        obarray.set_symbol_value(
            "completion-styles-alist",
            Value::list(vec![
                Value::list(vec![
                    Value::symbol("basic"),
                    Value::symbol("completion-basic-try-completion"),
                    Value::symbol("completion-basic-all-completions"),
                    Value::string(
                        "Completion of the prefix before point and the suffix after point.",
                    ),
                ]),
                Value::list(vec![
                    Value::symbol("partial-completion"),
                    Value::symbol("completion-pcm-try-completion"),
                    Value::symbol("completion-pcm-all-completions"),
                    Value::string("Completion of multiple words, each one taken as a prefix."),
                ]),
                Value::list(vec![
                    Value::symbol("emacs22"),
                    Value::symbol("completion-emacs22-try-completion"),
                    Value::symbol("completion-emacs22-all-completions"),
                    Value::string("Prefix completion that only operates on the text before point."),
                ]),
            ]),
        );
        obarray.set_symbol_value("completion-category-overrides", Value::NIL);
        obarray.set_symbol_value("completion-cycle-threshold", Value::NIL);
        obarray.set_symbol_value("completions-detailed", Value::NIL);
        obarray.set_symbol_value("completions-format", Value::symbol("horizontal"));
        obarray.set_symbol_value("completions-group", Value::NIL);
        obarray.set_symbol_value("completions-group-format", Value::string("     %s  "));
        obarray.set_symbol_value("completions-group-sort", Value::NIL);
        obarray.set_symbol_value(
            "completions-header-format",
            Value::string("%s possible completions:\n"),
        );
        obarray.set_symbol_value(
            "completions-highlight-face",
            Value::symbol("completions-highlight"),
        );
        obarray.set_symbol_value("completions-max-height", Value::NIL);
        obarray.set_symbol_value("completions-sort", Value::symbol("alphabetical"));
        obarray.set_symbol_value("completion-auto-help", Value::T);
        obarray.set_symbol_value("completion-auto-deselect", Value::T);
        obarray.set_symbol_value("completion-auto-select", Value::NIL);
        obarray.set_symbol_value("completion-auto-wrap", Value::T);
        obarray.set_symbol_value("completion-base-position", Value::NIL);
        obarray.set_symbol_value("completion-cycling", Value::NIL);
        obarray.set_symbol_value("completion-extra-properties", Value::NIL);
        obarray.set_symbol_value("completion-fail-discreetly", Value::NIL);
        obarray.set_symbol_value("completion-flex-nospace", Value::NIL);
        obarray.set_symbol_value("completion-in-region--data", Value::NIL);
        obarray.set_symbol_value(
            "completion-in-region-function",
            Value::symbol("completion--in-region"),
        );
        obarray.set_symbol_value("completion-in-region-functions", Value::NIL);
        obarray.set_symbol_value("completion-in-region-mode", Value::NIL);
        obarray.set_symbol_value("completion-in-region-mode--predicate", Value::NIL);
        obarray.set_symbol_value("completion-in-region-mode-hook", Value::NIL);
        obarray.set_symbol_value("completion-in-region-mode-predicate", Value::NIL);
        obarray.set_symbol_value("completion-show-help", Value::T);
        obarray.set_symbol_value("completion-show-inline-help", Value::T);
        obarray.set_symbol_value("completion-lazy-hilit", Value::NIL);
        obarray.set_symbol_value("completion-lazy-hilit-fn", Value::NIL);
        obarray.set_symbol_value(
            "completion-list-insert-choice-function",
            Value::symbol("completion--replace"),
        );
        obarray.set_symbol_value("completion-no-auto-exit", Value::NIL);
        obarray.set_symbol_value(
            "completion-pcm--delim-wild-regex",
            Value::string("[-_./:| *]"),
        );
        obarray.set_symbol_value("completion-pcm--regexp", Value::NIL);
        obarray.set_symbol_value(
            "completion-pcm-complete-word-inserts-delimiters",
            Value::NIL,
        );
        obarray.set_symbol_value("completion-pcm-word-delimiters", Value::string("-_./:| "));
        obarray.set_symbol_value("completion-reference-buffer", Value::NIL);
        obarray.set_symbol_value("completion-tab-width", Value::NIL);
        obarray.set_symbol_value("enable-recursive-minibuffers", Value::NIL);
        obarray.set_symbol_value("history-length", Value::fixnum(100));
        obarray.set_symbol_value("history-delete-duplicates", Value::NIL);
        obarray.set_symbol_value("history-add-new-input", Value::T);
        obarray.set_symbol_value("read-buffer-function", Value::NIL);
        obarray.set_symbol_value(
            "read-file-name-function",
            Value::symbol("read-file-name-default"),
        );
        obarray.set_symbol_value("read-expression-history", Value::NIL);
        obarray.set_symbol_value("read-number-history", Value::NIL);
        obarray.set_symbol_value("read-char-history", Value::NIL);
        obarray.set_symbol_value("read-answer-short", Value::symbol("auto"));
        obarray.set_symbol_value("read-char-by-name-sort", Value::NIL);
        obarray.set_symbol_value("read-char-choice-use-read-key", Value::NIL);
        obarray.set_symbol_value("read-circle", Value::T);
        obarray.make_special("read-circle");
        obarray.set_symbol_value("read-envvar-name-history", Value::NIL);
        obarray.set_symbol_value("read-face-name-sample-text", Value::string("SAMPLE"));
        obarray.set_symbol_value("read-key-delay", Value::make_float(0.01));
        obarray.set_symbol_value(
            "read-answer-map--memoize",
            Value::hash_table(HashTableTest::Equal),
        );
        obarray.set_symbol_value("read-extended-command-mode", Value::NIL);
        obarray.set_symbol_value("read-extended-command-mode-hook", Value::NIL);
        obarray.set_symbol_value("read-extended-command-predicate", Value::NIL);
        obarray.set_symbol_value("read-hide-char", Value::NIL);
        obarray.set_symbol_value("read-mail-command", Value::symbol("rmail"));
        obarray.set_symbol_value("read-minibuffer-restore-windows", Value::T);
        obarray.set_symbol_value("read-only-mode-hook", Value::NIL);
        obarray.set_symbol_value("read-process-output-max", Value::fixnum(65536));
        obarray.set_symbol_value("read-quoted-char-radix", Value::fixnum(8));
        obarray.set_symbol_value("read-regexp--case-fold", Value::NIL);
        obarray.set_symbol_value("read-regexp-defaults-function", Value::NIL);
        obarray.set_symbol_value("read-symbol-shorthands", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-frame-alist",
            Value::list(vec![
                Value::cons(Value::symbol("width"), Value::fixnum(80)),
                Value::cons(Value::symbol("height"), Value::fixnum(2)),
            ]),
        );
        obarray.set_symbol_value(
            "minibuffer-inactive-mode-abbrev-table",
            Value::symbol("minibuffer-inactive-mode-abbrev-table"),
        );
        obarray.set_symbol_value("minibuffer-inactive-mode-hook", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-inactive-mode-syntax-table",
            standard_syntax_table,
        );
        obarray.set_symbol_value(
            "minibuffer-mode-abbrev-table",
            Value::symbol("minibuffer-mode-abbrev-table"),
        );
        obarray.set_symbol_value("minibuffer-mode-hook", Value::NIL);
        obarray.set_symbol_value("minibuffer-local-map", minibuffer_local_map);
        obarray.set_symbol_value("minibuffer-local-filename-syntax", standard_syntax_table);
        obarray.set_symbol_value("minibuffer-history", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-history-variable",
            Value::symbol("minibuffer-history"),
        );
        obarray.set_symbol_value("minibuffer-history-position", Value::NIL);
        obarray.set_symbol_value("minibuffer-history-isearch-message-overlay", Value::NIL);
        obarray.set_symbol_value("minibuffer-history-search-history", Value::NIL);
        obarray.set_symbol_value("minibuffer-history-sexp-flag", Value::NIL);
        obarray.set_symbol_value("minibuffer-default", Value::NIL);
        obarray.set_symbol_value("minibuffer-default-add-done", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-default-add-function",
            Value::symbol("minibuffer-default-add-completions"),
        );
        obarray.set_symbol_value("minibuffer--original-buffer", Value::NIL);
        obarray.set_symbol_value("minibuffer--regexp-primed", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer--regexp-prompt-regexp",
            Value::string(
                "\\(?:Posix search\\|RE search\\|Search for regexp\\|Query replace regexp\\)",
            ),
        );
        obarray.set_symbol_value("minibuffer--require-match", Value::NIL);
        obarray.set_symbol_value("minibuffer-auto-raise", Value::NIL);
        obarray.set_symbol_value("minibuffer-follows-selected-frame", Value::T);
        obarray.set_symbol_value(
            "minibuffer-exit-hook",
            Value::list(vec![
                Value::symbol("minibuffer--regexp-exit"),
                Value::symbol("minibuffer-exit-on-screen-keyboard"),
                Value::symbol("minibuffer-restore-windows"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-completion-table", Value::NIL);
        obarray.set_symbol_value("minibuffer-completion-predicate", Value::NIL);
        obarray.set_symbol_value("minibuffer-completion-confirm", Value::NIL);
        obarray.set_symbol_value("minibuffer-completion-auto-choose", Value::T);
        obarray.set_symbol_value("minibuffer-completion-base", Value::NIL);
        obarray.set_symbol_value("minibuffer-help-form", Value::NIL);
        obarray.set_symbol_value("minibuffer-completing-file-name", Value::NIL);
        obarray.set_symbol_value("minibuffer-regexp-mode", Value::T);
        obarray.set_symbol_value("minibuffer-regexp-mode-hook", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-regexp-prompts",
            Value::list(vec![
                Value::string("Posix search"),
                Value::string("RE search"),
                Value::string("Search for regexp"),
                Value::string("Query replace regexp"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-message-clear-timeout", Value::NIL);
        obarray.set_symbol_value("minibuffer-message-overlay", Value::NIL);
        obarray.set_symbol_value("minibuffer-message-properties", Value::NIL);
        obarray.set_symbol_value("minibuffer-message-timeout", Value::fixnum(2));
        obarray.set_symbol_value("minibuffer-message-timer", Value::NIL);
        obarray.set_symbol_value("minibuffer-lazy-count-format", Value::string("%s "));
        obarray.set_symbol_value("minibuffer-text-before-history", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-prompt-properties",
            Value::list(vec![
                Value::symbol("read-only"),
                Value::T,
                Value::symbol("face"),
                Value::symbol("minibuffer-prompt"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-allow-text-properties", Value::NIL);
        obarray.set_symbol_value("minibuffer-scroll-window", Value::NIL);
        obarray.set_symbol_value("minibuffer-visible-completions", Value::NIL);
        obarray.set_symbol_value("minibuffer-visible-completions--always-bind", Value::NIL);
        obarray.set_symbol_value("minibuffer-depth-indicate-mode", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-default-prompt-format",
            Value::string(" (default %s)"),
        );
        obarray.set_symbol_value("minibuffer-beginning-of-buffer-movement", Value::NIL);
        obarray.set_symbol_value("minibuffer-electric-default-mode", Value::NIL);
        obarray.set_symbol_value("minibuffer-temporary-goal-position", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-confirm-exit-commands",
            Value::list(vec![
                Value::symbol("completion-at-point"),
                Value::symbol("minibuffer-complete"),
                Value::symbol("minibuffer-complete-word"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-history-case-insensitive-variables", Value::NIL);
        obarray.set_symbol_value("minibuffer-on-screen-keyboard-displayed", Value::NIL);
        obarray.set_symbol_value("minibuffer-on-screen-keyboard-timer", Value::NIL);
        obarray.set_symbol_value(
            "minibuffer-setup-hook",
            Value::list(vec![
                Value::symbol("rfn-eshadow-setup-minibuffer"),
                Value::symbol("minibuffer--regexp-setup"),
                Value::symbol("minibuffer-setup-on-screen-keyboard"),
                Value::symbol("minibuffer-error-initialize"),
                Value::symbol("minibuffer-history-isearch-setup"),
                Value::symbol("minibuffer-history-initialize"),
            ]),
        );
        obarray.set_symbol_value("regexp-search-ring", Value::NIL);
        obarray.set_symbol_value("regexp-search-ring-max", Value::fixnum(16));
        obarray.set_symbol_value("regexp-search-ring-yank-pointer", Value::NIL);
        obarray.set_symbol_value("search-ring", Value::NIL);
        obarray.set_symbol_value("search-ring-max", Value::fixnum(16));
        obarray.set_symbol_value("search-ring-update", Value::NIL);
        obarray.set_symbol_value("search-ring-yank-pointer", Value::NIL);
        obarray.set_symbol_value("last-abbrev", Value::NIL);
        obarray.set_symbol_value("last-abbrev-location", Value::fixnum(0));
        obarray.set_symbol_value("last-abbrev-text", Value::NIL);
        obarray.set_symbol_value("last-command-event", Value::NIL);
        // last-event-frame is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("last-event-device", Value::NIL);
        obarray.set_symbol_value("last-input-event", Value::NIL);
        obarray.set_symbol_value("last-nonmenu-event", Value::NIL);
        obarray.set_symbol_value("last-prefix-arg", Value::NIL);
        obarray.set_symbol_value("last-kbd-macro", Value::NIL);
        obarray.set_symbol_value("last-code-conversion-error", Value::NIL);
        obarray.set_symbol_value("last-coding-system-specified", Value::NIL);
        obarray.set_symbol_value("last-coding-system-used", Value::symbol("undecided-unix"));
        obarray.set_symbol_value("last-next-selection-coding-system", Value::NIL);
        obarray.set_symbol_value("command-debug-status", Value::NIL);
        obarray.set_symbol_value(
            "command-error-function",
            Value::symbol("help-command-error-confusable-suggestions"),
        );
        obarray.set_symbol_value("key-substitution-in-progress", Value::NIL);
        obarray.set_symbol_value("this-command", Value::NIL);
        obarray.set_symbol_value("real-this-command", Value::NIL);
        obarray.set_symbol_value("this-command-keys-shift-translated", Value::NIL);
        obarray.set_symbol_value("current-prefix-arg", Value::NIL);
        obarray.set_symbol_value("track-mouse", Value::NIL);
        obarray.make_special("track-mouse");
        obarray.set_symbol_value(
            "while-no-input-ignore-events",
            Value::list(vec![
                Value::symbol("thread-event"),
                Value::symbol("file-notify"),
                Value::symbol("dbus-event"),
                Value::symbol("select-window"),
                Value::symbol("help-echo"),
                Value::symbol("move-frame"),
                Value::symbol("iconify-frame"),
                Value::symbol("make-frame-visible"),
                Value::symbol("focus-in"),
                Value::symbol("focus-out"),
                Value::symbol("config-changed-event"),
                Value::symbol("selection-request"),
                Value::symbol("monitors-changed"),
            ]),
        );
        obarray.make_special("while-no-input-ignore-events");
        obarray.set_symbol_value("input-pending-p-filter-events", Value::T);
        obarray.make_special("input-pending-p-filter-events");
        obarray.set_symbol_value("deactivate-mark", Value::T);
        obarray.set_symbol_value("mark-active", Value::NIL);
        obarray.set_symbol_value("mark-even-if-inactive", Value::T);
        obarray.set_symbol_value("mark-ring", Value::NIL);
        obarray.set_symbol_value("mark-ring-max", Value::fixnum(16));
        // saved-region-selection is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("transient-mark-mode", Value::NIL);
        obarray.set_symbol_value("transient-mark-mode-hook", Value::NIL);
        obarray.set_symbol_value("post-select-region-hook", Value::NIL);
        obarray.set_symbol_value("echo-area-clear-hook", Value::NIL);
        obarray.set_symbol_value("display-monitors-changed-functions", Value::NIL);
        obarray.set_symbol_value("delete-terminal-functions", Value::NIL);
        obarray.set_symbol_value("suspend-tty-functions", Value::NIL);
        obarray.set_symbol_value("resume-tty-functions", Value::NIL);
        obarray.set_symbol_value("overriding-local-map", Value::NIL);
        obarray.make_special("overriding-local-map");
        obarray.set_symbol_value("overriding-local-map-menu-flag", Value::NIL);
        obarray.make_special("overriding-local-map-menu-flag");
        obarray.set_symbol_value("overriding-plist-environment", Value::NIL);
        obarray.set_symbol_value("overriding-terminal-local-map", Value::NIL);
        // GNU uses DEFVAR_KBOARD here. NeoVM does not yet split keyboard state
        // per terminal, so model it as a dynamically scoped runtime variable.
        obarray.make_special("overriding-terminal-local-map");
        obarray.set_symbol_value("overriding-text-conversion-style", Value::symbol("lambda"));

        // ---- C-level bootstrap variables required by loadup.el files ----

        // Standard keymaps (C creates these in keyboard.c:init_kboard)
        obarray.set_symbol_value("special-event-map", special_event_map);
        obarray.set_symbol_value(
            "mode-line-window-dedicated-keymap",
            mode_line_window_dedicated_keymap,
        );
        obarray.set_symbol_value("indent-rigidly-map", indent_rigidly_map);
        obarray.set_symbol_value("text-mode-map", text_mode_map);
        obarray.set_symbol_value("image-slice-map", image_slice_map);
        obarray.set_symbol_value("tool-bar-map", tool_bar_map);
        obarray.set_symbol_value("key-translation-map", key_translation_map);
        obarray.set_symbol_value("function-key-map", function_key_map);
        obarray.set_symbol_value("input-decode-map", input_decode_map);
        obarray.make_special("input-decode-map");
        obarray.set_symbol_value("local-function-key-map", local_function_key_map);
        obarray.make_special("local-function-key-map");
        obarray.set_symbol_value("keyboard-translate-table", Value::NIL);

        // Core eval variables (stay in eval.rs)
        obarray.set_symbol_value("purify-flag", Value::NIL);
        // GNU Emacs defaults to 1600 but only increments lisp_eval_depth in
        // eval_sub() and Ffuncall(). NeoVM increments depth for every
        // sub-expression including primitive calls (get, fboundp, etc.), so
        // the same Elisp code uses ~5x more depth units. Use 10000 to match
        // effective GNU depth capacity.
        obarray.set_symbol_value("max-lisp-eval-depth", Value::fixnum(2400));
        obarray.set_symbol_value("max-specpdl-size", Value::fixnum(1800));
        obarray.set_symbol_value("inhibit-load-charset-map", Value::NIL);

        // Terminal/display variables (C-level DEFVAR in official Emacs)
        obarray.set_symbol_value("standard-display-table", Value::NIL);
        obarray.set_symbol_value(
            "image-load-path",
            Value::list(vec![
                Value::string("/usr/share/emacs/30.1/etc/images/"),
                Value::symbol("data-directory"),
            ]),
        );
        obarray.set_symbol_value("image-scaling-factor", Value::make_float(1.0));

        // User init / startup (C DEFVAR in official Emacs)
        obarray.set_symbol_value("user-init-file", Value::NIL);
        obarray.set_symbol_value("user-emacs-directory", Value::string("~/.emacs.d/"));

        // Frame parameters (C DEFVAR in official Emacs)
        obarray.set_symbol_value("frame--special-parameters", Value::NIL);

        // Initialize distributed bootstrap variables
        super::alloc::register_bootstrap_vars(&mut obarray);
        super::load::register_bootstrap_vars(&mut obarray);
        super::fileio::register_bootstrap_vars(&mut obarray);
        super::window_cmds::register_bootstrap_vars(&mut obarray);
        super::keyboard::pure::register_bootstrap_vars(&mut obarray);
        super::composite::register_bootstrap_vars(&mut obarray);
        super::coding::register_bootstrap_vars(&mut obarray);
        super::xdisp::register_bootstrap_vars(&mut obarray);
        super::textprop::register_bootstrap_vars(&mut obarray);
        super::xfaces::register_bootstrap_vars(&mut obarray);
        super::frame_vars::register_bootstrap_vars(&mut obarray);
        super::buffer_vars::register_bootstrap_vars(&mut obarray);

        // ---- end C-level bootstrap variables ----

        obarray.set_symbol_value("unread-input-method-events", Value::NIL);
        obarray.set_symbol_value("unread-post-input-method-events", Value::NIL);
        obarray.set_symbol_value("input-method-alist", Value::NIL);
        obarray.set_symbol_value("input-method-activate-hook", Value::NIL);
        obarray.set_symbol_value("input-method-after-insert-chunk-hook", Value::NIL);
        obarray.set_symbol_value("input-method-deactivate-hook", Value::NIL);
        obarray.set_symbol_value("input-method-exit-on-first-char", Value::NIL);
        obarray.set_symbol_value("input-method-exit-on-invalid-key", Value::NIL);
        obarray.set_symbol_value("input-method-function", Value::symbol("list"));
        obarray.set_symbol_value("input-method-highlight-flag", Value::T);
        obarray.set_symbol_value("input-method-history", Value::NIL);
        // input-method-previous-message is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("input-method-use-echo-area", Value::NIL);
        obarray.set_symbol_value("input-method-verbose-flag", Value::symbol("default"));
        obarray.set_symbol_value("unread-command-events", Value::NIL);
        // GNU Emacs seeds core startup vars with integer
        // `variable-documentation` offsets in the DOC table.
        for &(name, _) in STARTUP_VARIABLE_DOC_STUBS {
            obarray.put_property(name, "variable-documentation", Value::fixnum(0));
        }
        // Some startup docs are string-valued in GNU Emacs (not integer offsets).
        for &(name, doc) in STARTUP_VARIABLE_DOC_STRING_PROPERTIES {
            obarray.put_property(name, "variable-documentation", Value::string(doc));
        }

        obarray.set_symbol_function("x-color-defined-p", Value::symbol("color-defined-p"));
        obarray.set_symbol_function("x-color-values", Value::symbol("color-values"));
        obarray.set_symbol_function("x-defined-colors", Value::symbol("defined-colors"));
        obarray.set_symbol_function("x-get-selection", Value::symbol("gui-get-selection"));
        obarray.set_symbol_function(
            "x-get-selection-value",
            Value::symbol("gui-get-primary-selection"),
        );
        obarray.set_symbol_function("x-select-text", Value::symbol("gui-select-text"));
        obarray.set_symbol_function("x-selection-value", Value::symbol("gui-selection-value"));
        obarray.set_symbol_function("x-set-selection", Value::symbol("gui-set-selection"));
        obarray.set_symbol_function(
            "window-inside-pixel-edges",
            Value::symbol("window-body-pixel-edges"),
        );
        obarray.set_symbol_function("window-inside-edges", Value::symbol("window-body-edges"));
        obarray.set_symbol_function("replace-rectangle", Value::symbol("string-rectangle"));
        // Bootstrap primitive function cells that GNU `simple.el` references
        // before its own Elisp defs overwrite them. Without these placeholders,
        // loaded GNU bytecode can capture `nil` for forward/runtime calls into
        // Builtin function cells are set by defsubr() during init_builtins().
        for name in ["mark-marker", "region-beginning", "region-end"] {
            obarray.set_symbol_function(name, Value::subr(intern(name)));
        }
        // GNU Emacs exposes this helper as a Lisp wrapper, not a primitive.
        obarray.set_symbol_function(
            "subr-primitive-p",
            Value::make_bytecode(Compiler::new(false).compile_lambda(
                &LambdaParams::simple(vec![intern("object")]),
                &[Expr::List(vec![
                    Expr::Symbol(intern("subrp")),
                    Expr::Symbol(intern("object")),
                ])],
            )),
        );
        // Bookmark command wrappers are startup autoloads in GNU Emacs.
        let seed_autoload = |name: &str, file: &str, doc: &str| {
            obarray.set_symbol_function(
                name,
                Value::list(vec![
                    Value::symbol("autoload"),
                    Value::string(file),
                    Value::string(doc),
                    Value::T,
                    Value::NIL,
                ]),
            );
        };
        drop(seed_autoload);
        let mut seed_autoload_noninteractive = |name: &str, file: &str, doc: &str| {
            obarray.set_symbol_function(
                name,
                Value::list(vec![
                    Value::symbol("autoload"),
                    Value::string(file),
                    Value::string(doc),
                    Value::NIL,
                    Value::NIL,
                ]),
            );
        };
        // Some helper autoloads are non-interactive in GNU Emacs startup
        // function-cells; override their startup metadata accordingly.
        seed_autoload_noninteractive(
            "substitute-command-keys",
            "help",
            "Replace key descriptions in STRING.",
        );
        seed_autoload_noninteractive(
            "wholenump",
            "subr",
            "Return non-nil if OBJECT is an integer greater than or equal to zero.",
        );
        seed_autoload_noninteractive(
            "window-height",
            "window",
            "Return the total height, in lines, of WINDOW.",
        );
        seed_autoload_noninteractive(
            "window-width",
            "window",
            "Return the width, in columns, of WINDOW.",
        );
        // Keep these as non-interactive autoload wrappers to match GNU Emacs
        // `symbol-function` shape during bootstrap.
        drop(seed_autoload_noninteractive);
        obarray.set_symbol_function(
            "string-chop-newline",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string("Remove the final newline (if any) from STRING."),
                Value::NIL,
                Value::NIL,
            ]),
        );
        obarray.set_symbol_function(
            "string-pad",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string("Pad STRING to LENGTH using PADDING."),
                Value::NIL,
                Value::NIL,
            ]),
        );
        obarray.set_symbol_function(
            "string-fill",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string(
                    "Try to word-wrap STRING so that it displays with lines no wider than WIDTH.",
                ),
                Value::NIL,
                Value::NIL,
            ]),
        );
        obarray.set_symbol_function(
            "string-limit",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string(
                    "Return a substring of STRING that is (up to) LENGTH characters long.",
                ),
                Value::NIL,
                Value::NIL,
            ]),
        );
        // Some startup helpers are Lisp functions that delegate to primitives.
        // Seed lightweight bytecode wrappers so `symbol-function` shape matches GNU Emacs.
        let seed_function_wrapper = |obarray: &mut Obarray, name: &str| {
            let wrapper = format!("neovm--startup-subr-wrapper-{name}");
            obarray.set_symbol_function(&wrapper, Value::subr(intern(name)));

            let params = LambdaParams {
                required: vec![],
                optional: vec![],
                rest: Some(intern("args")),
            };
            let body = vec![Expr::List(vec![
                Expr::Symbol(intern("apply")),
                Expr::List(vec![
                    Expr::Symbol(intern("quote")),
                    Expr::Symbol(intern(&wrapper)),
                ]),
                Expr::Symbol(intern("args")),
            ])];
            let bc = Compiler::new(false).compile_lambda(&params, &body);
            obarray.set_symbol_function(name, Value::make_bytecode(bc));
        };
        for name in ["string-blank-p", "string-empty-p"] {
            seed_function_wrapper(&mut obarray, name);
        }

        // `word-at-point` is defined in GNU Emacs Lisp by `thingatpt.el`,
        // not as a startup builtin.
        obarray.clear_function_silent("word-at-point");

        let noop_macro = Value::make_macro(LambdaData {
            params: LambdaParams {
                required: Vec::new(),
                optional: Vec::new(),
                rest: Some(intern("_args")),
            },
            body: vec![].into(), // empty body → nil
            env: None,
            docstring: None,
            doc_form: None,
            interactive: None,
        });

        // cl-defgeneric and cl-defmethod stubs — these macros are normally
        // defined by cl-generic.el, which fails during bootstrap (needs cl
        // type system).  Stub them as no-ops so files like startup.el and
        // frame.el that use them can still load.
        for stub_name in &["cl-defgeneric", "cl-defmethod"] {
            obarray.set_symbol_function(stub_name, noop_macro);
        }

        // cl-check-type and cl-typep stubs — cl-preloaded.el uses
        // (cl-check-type ...) which macroexpands to (cl-typep val type).
        // cl-macs.el defines these via define-inline (stores inline body
        // in the variable cell), but cl-macs is only eval-when-compile'd.
        // Stub cl-check-type as a no-op macro and cl-typep as a function
        // returning t — skips type validation during bootstrap.
        obarray.set_symbol_function("cl-check-type", noop_macro);
        obarray.set_symbol_function(
            "cl-typep",
            Value::make_lambda(LambdaData {
                params: LambdaParams {
                    required: vec![intern("_val"), intern("_type")],
                    optional: Vec::new(),
                    rest: None,
                },
                body: vec![Expr::Symbol(intern("t"))].into(),
                env: None,
                docstring: None,
                doc_form: None,
                interactive: None,
            }),
        );
        obarray.set_symbol_value("cl-typep", Value::T);

        // Mark standard variables as special (dynamically bound)
        for name in &[
            "debug-on-error",
            "debugger",
            "lexical-binding",
            "load-prefer-newer",
            "load-path",
            "load-history",
            "default-directory",
            "load-file-name",
            "set-auto-coding-for-load",
            "noninteractive",
            "inhibit-quit",
            "inhibit-read-only",
            "internal-make-interpreted-closure-function",
            "print-length",
            "print-level",
            "standard-output",
            "case-fold-search",
            "buffer-read-only",
            "current-prefix-arg",
            "prefix-arg",
            "last-prefix-arg",
            "last-command-event",
            "last-input-event",
            "last-command",
            "real-last-command",
            "this-command",
            "real-this-command",
            "this-command-keys-shift-translated",
            "unread-command-events",
            "unread-input-method-events",
            "unread-post-input-method-events",
            // transient-mark-mode is a C-level variable in GNU (buffer.c),
            // always dynamically scoped. Must be special so (let ((transient-mark-mode t)) ...)
            // creates a dynamic binding visible to called functions like region-active-p.
            "transient-mark-mode",
        ] {
            obarray.make_special(name);
        }

        // Initialize the standard error hierarchy (error, user-error, etc.)
        super::errors::init_standard_errors(&mut obarray);

        // Initialize indentation variables (tab-width, indent-tabs-mode, etc.)
        super::indent::init_indent_vars(&mut obarray);

        let mut custom = CustomManager::new();
        super::textprop::init_textprop_vars(&mut obarray, &mut custom);
        super::syntax::init_syntax_vars(&mut obarray, &mut custom);
        // Register all DEFVAR_PER_BUFFER variables from GNU Emacs buffer.c.
        // These are C-level buffer-local variables that must exist before
        // any .el file loads.  Default values match init_buffer_once().
        macro_rules! defvar_per_buffer {
            ($name:expr, $val:expr) => {
                custom.make_variable_buffer_local($name);
                obarray.make_special($name);
                obarray.set_symbol_value($name, $val);
                obarray.make_buffer_local($name, true);
            };
        }
        {
            // Core buffer identity
            defvar_per_buffer!("buffer-file-name", Value::NIL);
            defvar_per_buffer!("buffer-file-truename", Value::NIL);
            // GNU buffer.c:5381 — default-directory defaults to cwd.
            // This sets the GLOBAL default; new buffers inherit it.
            {
                let cwd = std::env::current_dir()
                    .map(|p| {
                        let mut s = p.to_string_lossy().into_owned();
                        if !s.ends_with('/') {
                            s.push('/');
                        }
                        s
                    })
                    .unwrap_or_else(|_| "/".to_string());
                defvar_per_buffer!("default-directory", Value::string(cwd));
            }
            defvar_per_buffer!("buffer-read-only", Value::NIL);
            defvar_per_buffer!("buffer-undo-list", Value::NIL);
            defvar_per_buffer!("buffer-saved-size", Value::fixnum(0));
            defvar_per_buffer!("buffer-backed-up", Value::NIL);
            defvar_per_buffer!("buffer-file-format", Value::NIL);
            defvar_per_buffer!("buffer-auto-save-file-name", Value::NIL);
            defvar_per_buffer!("buffer-auto-save-file-format", Value::T);
            defvar_per_buffer!("buffer-file-coding-system", Value::NIL);
            defvar_per_buffer!("buffer-display-count", Value::fixnum(0));
            defvar_per_buffer!("buffer-display-time", Value::NIL);

            // Modes
            defvar_per_buffer!("major-mode", Value::symbol("fundamental-mode"));
            defvar_per_buffer!("mode-name", Value::NIL);
            defvar_per_buffer!("mode-line-format", Value::string("%-"));
            defvar_per_buffer!("header-line-format", Value::NIL);
            defvar_per_buffer!("tab-line-format", Value::NIL);
            defvar_per_buffer!("local-abbrev-table", Value::NIL);
            defvar_per_buffer!("local-minor-modes", Value::NIL);
            defvar_per_buffer!("abbrev-mode", Value::NIL);
            defvar_per_buffer!("overwrite-mode", Value::NIL);
            defvar_per_buffer!("auto-fill-function", Value::NIL);

            // Search (GNU buffer.c DEFVAR_PER_BUFFER)
            defvar_per_buffer!("case-fold-search", Value::T);
            defvar_per_buffer!("indent-tabs-mode", Value::T);

            // Display
            defvar_per_buffer!("tab-width", Value::fixnum(8));
            defvar_per_buffer!("fill-column", Value::fixnum(70));
            defvar_per_buffer!("left-margin", Value::fixnum(0));
            defvar_per_buffer!("truncate-lines", Value::NIL);
            defvar_per_buffer!("word-wrap", Value::NIL);
            defvar_per_buffer!("ctl-arrow", Value::T);
            defvar_per_buffer!("selective-display", Value::NIL);
            defvar_per_buffer!("selective-display-ellipses", Value::T);
            defvar_per_buffer!("enable-multibyte-characters", Value::T);
            defvar_per_buffer!("buffer-display-table", Value::NIL);
            defvar_per_buffer!("buffer-invisibility-spec", Value::NIL);
            defvar_per_buffer!("line-spacing", Value::NIL);
            defvar_per_buffer!("cache-long-scans", Value::T);
            defvar_per_buffer!("point-before-scroll", Value::NIL);

            // Cursor
            defvar_per_buffer!("cursor-type", Value::T);
            defvar_per_buffer!("cursor-in-non-selected-windows", Value::T);

            // Marks
            defvar_per_buffer!("mark-active", Value::NIL);

            // Bidi
            defvar_per_buffer!("bidi-display-reordering", Value::T);
            defvar_per_buffer!("bidi-paragraph-direction", Value::NIL);
            defvar_per_buffer!("bidi-paragraph-start-re", Value::NIL);
            defvar_per_buffer!("bidi-paragraph-separate-re", Value::NIL);

            // Fringes and margins
            defvar_per_buffer!("left-fringe-width", Value::NIL);
            defvar_per_buffer!("right-fringe-width", Value::NIL);
            defvar_per_buffer!("left-margin-width", Value::fixnum(0));
            defvar_per_buffer!("right-margin-width", Value::fixnum(0));
            defvar_per_buffer!("fringes-outside-margins", Value::NIL);
            defvar_per_buffer!("fringe-indicator-alist", Value::NIL);
            defvar_per_buffer!("fringe-cursor-alist", Value::NIL);
            defvar_per_buffer!("indicate-empty-lines", Value::NIL);
            defvar_per_buffer!("indicate-buffer-boundaries", Value::NIL);

            // Scroll bars
            defvar_per_buffer!("scroll-bar-width", Value::NIL);
            defvar_per_buffer!("scroll-bar-height", Value::NIL);
            defvar_per_buffer!("vertical-scroll-bar", Value::T);
            defvar_per_buffer!("horizontal-scroll-bar", Value::T);
            defvar_per_buffer!("scroll-up-aggressively", Value::NIL);
            defvar_per_buffer!("scroll-down-aggressively", Value::NIL);

            // Other
            defvar_per_buffer!("text-conversion-style", Value::NIL);
        }

        let mut command_loop = crate::keyboard::CommandLoop::new();
        command_loop
            .keyboard
            .set_terminal_translation_maps(input_decode_map, local_function_key_map);

        let mut ev = Self {
            subr_registry: Vec::new(),
            interner,
            tagged_heap,
            obarray,
            specpdl: Vec::new(),
            lexenv: Value::NIL,
            internal_interpreter_environment_symbol,
            quit_flag_symbol,
            inhibit_quit_symbol,
            throw_on_input_symbol,
            kill_emacs_symbol,
            features: Vec::new(),
            require_stack: Vec::new(),
            loads_in_progress: Vec::new(),
            buffers: BufferManager::new(),
            match_data: None,
            processes: ProcessManager::new(),
            timers: TimerManager::new(),
            watchers: VariableWatcherList::new(),
            standard_syntax_table,
            standard_category_table,
            current_local_map: Value::NIL,
            registers: RegisterManager::new(),
            bookmarks: BookmarkManager::new(),
            abbrevs: AbbrevManager::new(),
            autoloads: AutoloadManager::new(),
            custom,
            rectangle: RectangleState::new(),
            interactive: InteractiveRegistry::new(),
            minibuffers: MinibufferManager::new(),
            current_message: None,
            minibuffer_selected_window: None,
            active_minibuffer_window: None,
            shutdown_request: None,
            input_mode_interrupt: true,
            quit_char: 7,
            waiting_for_user_input: false,
            frames: FrameManager::new(),
            modes: ModeRegistry::new(),
            threads: ThreadManager::new(),
            kmacro: KmacroManager::new(),
            command_loop,
            input_rx: None,
            #[cfg(unix)]
            wakeup_fd: None,
            redisplay_fn: None,
            display_host: None,
            coding_systems: CodingSystemManager::new(),
            face_table: FaceTable::new(),
            face_change_count: 0,
            depth: 0,
            eval_counter: 0,
            max_depth: 2400, // Matches GNU Emacs default (max-lisp-eval-depth)
            gc_pending: false,
            gc_count: 0,
            gc_inhibit_depth: 0,
            gc_stress: false,
            temp_roots: Vec::new(),
            vm_gc_roots: Vec::new(),
            runtime_backtrace: Vec::new(),
            condition_stack: Vec::new(),
            next_resume_id: 1,
            pending_safe_funcalls: Vec::new(),
            saved_lexenvs: Vec::new(),
            named_call_cache: Vec::with_capacity(NAMED_CALL_CACHE_CAPACITY),
            source_literal_cache: HashMap::new(),
            macro_expansion_cache: HashMap::new(),
            macro_cache_hits: 0,
            macro_cache_misses: 0,
            macro_expand_total_us: 0,
            macro_cache_disabled: false,
            interpreted_closure_filter_fn: None,
            interpreted_closure_trim_cache: HashMap::new(),
        };
        // The heap and interner are boxed so their addresses are stable across moves.
        // Re-point anyway to be explicit about thread-local state.
        set_current_interner(&mut ev.interner);
        crate::tagged::gc::set_tagged_heap(&mut ev.tagged_heap);
        super::syntax::restore_standard_syntax_table_object(ev.standard_syntax_table);
        super::category::restore_standard_category_table_object(ev.standard_category_table);
        ev.sync_thread_runtime_bindings();
        ev.sync_current_thread_buffer_state();
        ev
    }

    // -----------------------------------------------------------------------
    // pdump reconstruction
    // -----------------------------------------------------------------------

    /// Reconstruct an Context from pdump data.
    ///
    /// Thread-local pointers (CURRENT_INTERNER, CURRENT_HEAP) and caches
    /// must already be set by the caller before calling this.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_dump(
        interner: Box<StringInterner>,
        obarray: Obarray,
        lexenv: Value,
        features: Vec<SymId>,
        require_stack: Vec<SymId>,
        buffers: BufferManager,
        autoloads: AutoloadManager,
        custom: CustomManager,
        modes: ModeRegistry,
        coding_systems: CodingSystemManager,
        face_table: FaceTable,
        abbrevs: AbbrevManager,
        interactive: InteractiveRegistry,
        rectangle: RectangleState,
        standard_syntax_table: Value,
        standard_category_table: Value,
        current_local_map: Value,
        kmacro: KmacroManager,
        registers: RegisterManager,
        bookmarks: BookmarkManager,
        watchers: VariableWatcherList,
    ) -> Self {
        let dumped_function_surface = obarray.clone();
        let mut obarray = obarray;
        obarray.intern("internal-interpreter-environment");
        let internal_interpreter_environment_symbol =
            intern_uninterned("internal-interpreter-environment");
        obarray.set_symbol_value_id(internal_interpreter_environment_symbol, Value::NIL);
        obarray.make_special_id(internal_interpreter_environment_symbol);
        let quit_flag_symbol = intern("quit-flag");
        obarray.make_special_id(quit_flag_symbol);
        let inhibit_quit_symbol = intern("inhibit-quit");
        obarray.make_special_id(inhibit_quit_symbol);
        let throw_on_input_symbol = intern("throw-on-input");
        obarray.make_special_id(throw_on_input_symbol);
        let kill_emacs_symbol = intern("kill-emacs");
        let mut tagged_heap = Box::new(crate::tagged::gc::TaggedHeap::new());
        crate::tagged::gc::set_tagged_heap(&mut tagged_heap);

        let mut ev = Self {
            subr_registry: Vec::new(),
            interner,
            tagged_heap,
            obarray,
            specpdl: Vec::new(),
            lexenv,
            internal_interpreter_environment_symbol,
            quit_flag_symbol,
            inhibit_quit_symbol,
            throw_on_input_symbol,
            kill_emacs_symbol,
            features,
            require_stack,
            loads_in_progress: Vec::new(),
            buffers,
            match_data: None,
            processes: ProcessManager::new(),
            timers: TimerManager::new(),
            watchers,
            standard_syntax_table,
            standard_category_table,
            current_local_map,
            registers,
            bookmarks,
            abbrevs,
            autoloads,
            custom,
            rectangle,
            interactive,
            minibuffers: MinibufferManager::new(),
            current_message: None,
            minibuffer_selected_window: None,
            active_minibuffer_window: None,
            shutdown_request: None,
            input_mode_interrupt: true,
            quit_char: 7,
            waiting_for_user_input: false,
            frames: FrameManager::new(),
            modes,
            threads: ThreadManager::new(),
            kmacro,
            command_loop: crate::keyboard::CommandLoop::new(),
            input_rx: None,
            #[cfg(unix)]
            wakeup_fd: None,
            redisplay_fn: None,
            display_host: None,
            coding_systems,
            face_table,
            face_change_count: 0,
            depth: 0,
            eval_counter: 0,
            max_depth: 2400,
            gc_pending: false,
            gc_count: 0,
            gc_inhibit_depth: 0,
            gc_stress: false,
            temp_roots: Vec::new(),
            vm_gc_roots: Vec::new(),
            runtime_backtrace: Vec::new(),
            condition_stack: Vec::new(),
            next_resume_id: 1,
            pending_safe_funcalls: Vec::new(),
            saved_lexenvs: Vec::new(),
            named_call_cache: Vec::with_capacity(NAMED_CALL_CACHE_CAPACITY),
            source_literal_cache: HashMap::new(),
            macro_expansion_cache: HashMap::new(),
            macro_cache_hits: 0,
            macro_cache_misses: 0,
            macro_expand_total_us: 0,
            macro_cache_disabled: false,
            interpreted_closure_filter_fn: None,
            interpreted_closure_trim_cache: HashMap::new(),
        };
        // Re-point thread-local pointers to the evaluator's owned boxes.
        set_current_interner(&mut ev.interner);
        crate::tagged::gc::set_tagged_heap(&mut ev.tagged_heap);
        // Set stack bottom for conservative GC stack scanning (same as Context::new).
        #[cfg(target_os = "linux")]
        {
            if let Some(stack_end) = crate::tagged::gc::read_stack_end_from_proc() {
                ev.tagged_heap.set_stack_bottom(stack_end as *const u8);
            }
        }
        super::syntax::restore_standard_syntax_table_object(ev.standard_syntax_table);
        super::category::restore_standard_category_table_object(ev.standard_category_table);

        // Rebuild the builtin subr registry after pdump restore. The dumped
        // obarray already carries the authoritative runtime function-cell
        // surface, so restore that surface immediately afterward.
        builtins::init_builtins(&mut ev);
        for (sym_id, symbol) in dumped_function_surface.iter_symbols() {
            match symbol.function.as_ref() {
                Some(function) => ev.obarray.set_symbol_function_id(*sym_id, *function),
                None if dumped_function_surface.is_function_unbound_id(*sym_id) => {
                    ev.obarray.fmakunbound_id(*sym_id);
                }
                None => ev.obarray.clear_function_silent_id(*sym_id),
            }
        }

        ev.sync_keyboard_runtime_from_obarray();
        ev.sync_thread_runtime_bindings();
        ev.sync_current_thread_buffer_state();

        ev
    }

    // -----------------------------------------------------------------------
    // Garbage collection
    // -----------------------------------------------------------------------

    /// Enumerate every live `Value` reference in the evaluator and all
    /// sub-managers.  This is the root set for mark-and-sweep collection.
    fn collect_roots(&self) -> Vec<Value> {
        let mut roots = Vec::new();

        // Direct Context fields
        roots.extend(self.temp_roots.iter().cloned());
        roots.extend(self.vm_gc_roots.iter().cloned());
        for frame in &self.condition_stack {
            match frame {
                ConditionFrame::Catch { tag, .. } => roots.push(*tag),
                ConditionFrame::ConditionCase { conditions, .. } => roots.push(*conditions),
                ConditionFrame::HandlerBind {
                    conditions,
                    handler,
                    ..
                } => {
                    roots.push(*conditions);
                    roots.push(*handler);
                }
                ConditionFrame::SkipConditions { .. } => {}
            }
        }
        // Root old_value entries on the specpdl so GC doesn't collect them.
        for entry in &self.specpdl {
            match entry {
                SpecBinding::Let {
                    old_value: Some(val),
                    ..
                } => roots.push(*val),
                SpecBinding::LetLocal { old_value, .. } => roots.push(*old_value),
                SpecBinding::LetDefault {
                    old_value: Some(val),
                    ..
                } => roots.push(*val),
                _ => {}
            }
        }
        roots.push(self.lexenv);
        // Scan saved lexenvs (from apply_lambda's lexenv replacement)
        for saved_env in &self.saved_lexenvs {
            roots.push(*saved_env);
        }

        // Literal cache — cached quote_to_value results for pcase eq-memoization
        roots.extend(self.source_literal_cache.values().copied());

        // OpaqueValuePool — root all pooled Values (replaces per-entry opaque_roots)
        OPAQUE_POOL.with(|pool| pool.borrow().trace_roots(&mut roots));

        if let Some(filter_fn) = self.interpreted_closure_filter_fn {
            roots.push(filter_fn);
        }

        // Named call cache — holds a Value when target is Obarray(val)
        for cache in &self.named_call_cache {
            if let NamedCallTarget::Obarray(val) = &cache.target {
                roots.push(*val);
            }
        }
        for frame in &self.runtime_backtrace {
            roots.push(frame.function);
            roots.extend(frame.args.iter().copied());
        }
        for funcall in &self.pending_safe_funcalls {
            roots.push(funcall.function);
            roots.extend(funcall.args.iter().copied());
        }
        // Thread-local statics holding Values
        collect_thread_local_gc_roots(&mut roots);

        // current_local_map is a cons-list keymap Value, trace it as a root
        if !self.current_local_map.is_nil() {
            roots.push(self.current_local_map);
        }

        // Sub-managers
        self.obarray.trace_roots(&mut roots);
        self.processes.trace_roots(&mut roots);
        self.timers.trace_roots(&mut roots);
        self.watchers.trace_roots(&mut roots);
        self.registers.trace_roots(&mut roots);
        self.custom.trace_roots(&mut roots);
        self.autoloads.trace_roots(&mut roots);
        self.buffers.trace_roots(&mut roots);
        self.threads.trace_roots(&mut roots);
        self.kmacro.trace_roots(&mut roots);
        crate::gc::GcTrace::trace_roots(&self.command_loop, &mut roots);
        self.modes.trace_roots(&mut roots);
        self.frames.trace_roots(&mut roots);
        self.coding_systems.trace_roots(&mut roots);

        // Match data — SearchedString::Heap holds a live string Value
        if let Some(ref md) = self.match_data {
            if let Some(crate::emacs_core::regex::SearchedString::Heap(val)) = &md.searched_string {
                roots.push(*val);
            }
        }

        roots
    }

    /// Get the current GC threshold.
    pub fn gc_threshold(&self) -> usize {
        self.tagged_heap.gc_threshold()
    }

    /// Set the GC threshold. Use usize::MAX to effectively disable GC.
    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.tagged_heap.set_gc_threshold(threshold);
    }

    /// Set the maximum eval recursion depth.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// Set the thread-local interner and heap pointers for the current thread.
    ///
    /// Must be called when using an Context from a thread other than the one
    /// that created it (e.g., in worker thread pools).
    pub fn setup_thread_locals(&mut self) {
        set_current_interner(&mut self.interner);
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        super::syntax::restore_standard_syntax_table_object(self.standard_syntax_table);
        super::category::restore_standard_category_table_object(self.standard_category_table);
    }

    pub(crate) fn sync_current_thread_buffer_state(&mut self) {
        let current_thread_id = self.threads.current_thread_id();
        let current_buffer_id = self.buffers.current_buffer_id();
        self.threads
            .set_thread_current_buffer(current_thread_id, current_buffer_id);
    }

    fn sync_current_buffer_runtime_state(&mut self) -> Result<(), Flow> {
        self.sync_current_thread_buffer_state();
        super::casetab::sync_current_buffer_case_table_state(self)?;
        super::syntax::sync_current_buffer_syntax_table_state(self)?;
        Ok(())
    }

    pub(crate) fn switch_current_buffer(
        &mut self,
        id: crate::buffer::BufferId,
    ) -> Result<(), Flow> {
        if !self.buffers.switch_current(id) {
            return Err(signal(
                "error",
                vec![Value::string("Selecting deleted buffer")],
            ));
        }
        self.sync_current_buffer_runtime_state()
    }

    pub fn restore_current_buffer_if_live(&mut self, id: crate::buffer::BufferId) {
        if self.buffers.get(id).is_none() {
            return;
        }
        let _ = self.buffers.switch_current(id);
        let _ = self.sync_current_buffer_runtime_state();
    }

    /// Connect the input system for interactive mode.
    ///
    /// This mirrors GNU Emacs's `init_keyboard()` — it connects the evaluator
    /// to the render thread's input channel so that `read_char()` can block
    /// waiting for user input instead of returning immediately (batch mode).
    ///
    /// # Arguments
    /// * `input_rx` — Receiver end of the crossbeam channel from the render thread
    /// * `wakeup_fd` — Read end of the wakeup pipe (render thread writes to signal input)
    #[cfg(unix)]
    pub fn init_input_system(
        &mut self,
        input_rx: crossbeam_channel::Receiver<crate::keyboard::InputEvent>,
        wakeup_fd: std::os::unix::io::RawFd,
    ) {
        self.input_rx = Some(input_rx);
        self.wakeup_fd = Some(wakeup_fd);
        self.command_loop.running = true;
    }

    pub fn set_display_host(&mut self, host: Box<dyn DisplayHost>) {
        self.display_host = Some(host);
    }

    // -----------------------------------------------------------------------
    // Command loop (mirrors keyboard.c)
    // -----------------------------------------------------------------------

    /// Enter a recursive edit level.
    ///
    /// Mirrors GNU Emacs `Frecursive_edit()` (keyboard.c:772).
    /// Increments recursive depth, enters the command loop, decrements on exit.
    /// If the command loop exits via `abort-recursive-edit` (throw 'exit t),
    /// signals quit.  If via `exit-recursive-edit` (throw 'exit nil), returns
    /// normally.
    ///
    /// In batch mode (no input_rx), returns nil immediately.
    /// Enter a recursive edit level (public API).
    ///
    /// Returns `Ok(())` on normal exit, `Err(description)` on error.
    #[tracing::instrument(skip_all)]
    pub fn recursive_edit(&mut self) -> Result<(), String> {
        match self.recursive_edit_inner() {
            Ok(_) => Ok(()),
            Err(flow) => Err(format!("{:?}", flow)),
        }
    }

    pub(crate) fn request_shutdown(&mut self, exit_code: i32, restart: bool) {
        self.shutdown_request = Some(ShutdownRequest { exit_code, restart });
        self.command_loop.running = false;
    }

    pub fn shutdown_request(&self) -> Option<ShutdownRequest> {
        self.shutdown_request
    }

    #[tracing::instrument(skip_all, fields(depth = self.command_loop.recursive_depth, has_input = self.input_rx.is_some()))]
    pub(crate) fn recursive_edit_inner(&mut self) -> EvalResult {
        self.run_exit_wrapped_command_loop(true)
    }

    #[tracing::instrument(skip_all, fields(depth = self.command_loop.recursive_depth, has_input = self.input_rx.is_some()))]
    pub(crate) fn minibuffer_command_loop_inner(&mut self) -> EvalResult {
        self.run_exit_wrapped_command_loop(false)
    }

    fn run_exit_wrapped_command_loop(&mut self, increment_depth: bool) -> EvalResult {
        // Batch mode: no interactive input, return immediately.
        if self.input_rx.is_none() {
            tracing::info!("recursive_edit_inner: batch mode, returning immediately");
            return Ok(Value::NIL);
        }

        if increment_depth {
            self.command_loop.recursive_depth += 1;
        }

        // Register catch tag for 'exit (mirrors keyboard.c catch handler).
        let exit_tag = Value::symbol("exit");
        self.push_condition_frame(ConditionFrame::Catch {
            tag: exit_tag,
            resume: ResumeTarget::CommandLoopExit,
        });

        let result = self.command_loop_inner();

        self.pop_condition_frame();
        if increment_depth {
            self.command_loop.recursive_depth -= 1;
        }

        match result {
            Ok(val) => Ok(val),
            // exit-recursive-edit: throw 'exit nil → normal return
            Err(Flow::Throw { ref tag, ref value }) if tag.is_symbol_named("exit") => {
                if value.is_truthy() {
                    // abort-recursive-edit: throw 'exit t → signal quit
                    Err(super::error::signal("quit", vec![]))
                } else {
                    Ok(Value::NIL)
                }
            }
            Err(flow) => Err(flow),
        }
    }

    /// Inner command loop with top-level catch.
    ///
    /// Mirrors GNU Emacs `command_loop()` (keyboard.c:1104).
    /// Wraps command_loop_2 in a catch for 'top-level.
    #[tracing::instrument(skip_all)]
    fn command_loop_inner(&mut self) -> EvalResult {
        let outermost_command_loop =
            self.command_loop.recursive_depth == 1 && self.minibuffers.depth() == 0;
        loop {
            // Catch 'top-level throws (from (top-level) function).
            let top_level_tag = Value::symbol("top-level");
            self.push_condition_frame(ConditionFrame::Catch {
                tag: top_level_tag,
                resume: ResumeTarget::CommandLoopTopLevel,
            });

            let result = if outermost_command_loop {
                match self.command_loop_top_level_1() {
                    Ok(_) if self.command_loop.running => self.command_loop_2(),
                    Ok(_) => Ok(Value::NIL),
                    Err(flow) => Err(flow),
                }
            } else {
                self.command_loop_2()
            };

            self.pop_condition_frame();

            match result {
                // top-level throw → restart the loop
                Err(Flow::Throw { ref tag, .. }) if tag.is_symbol_named("top-level") => {
                    continue;
                }
                // Any other result propagates up
                other => return other,
            }
        }
    }

    fn command_loop_top_level_1(&mut self) -> EvalResult {
        let top_level = self
            .obarray
            .symbol_value("top-level")
            .copied()
            .unwrap_or(Value::NIL);

        eprintln!("command_loop_top_level_1: top-level={}", top_level);

        if top_level.is_nil() {
            eprintln!("command_loop_top_level_1: top-level is nil, skipping");
            self.log_startup_state("top-level-nil");
            return Ok(Value::NIL);
        }

        eprintln!("command_loop_top_level_1: evaluating top-level form");
        self.log_startup_state("top-level-before");
        match self.eval_value(&top_level) {
            Ok(_) => {
                eprintln!("command_loop_top_level_1: top-level completed OK");
                self.log_startup_state("top-level-after");
                Ok(Value::NIL)
            }
            Err(Flow::Signal(sig)) => {
                eprintln!(
                    "command_loop_top_level_1: top-level SIGNALED: {} {:?}",
                    sig.symbol_name(),
                    sig.data
                );
                let data_str = sig
                    .data
                    .iter()
                    .map(|value| format!("{value}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let error_msg = if data_str.is_empty() {
                    sig.symbol_name().to_string()
                } else {
                    format!("{}: {}", sig.symbol_name(), data_str)
                };
                if cfg!(test) {
                    let last_phase = self
                        .obarray
                        .symbol_value("neomacs--startup-last-phase")
                        .copied()
                        .map(|value| crate::emacs_core::print_value_with_eval(self, &value))
                        .unwrap_or_else(|| "nil".to_string());
                    let last_call = self
                        .obarray
                        .symbol_value("neomacs--startup-last-call")
                        .copied()
                        .map(|value| crate::emacs_core::print_value_with_eval(self, &value))
                        .unwrap_or_else(|| "nil".to_string());
                    eprintln!(
                        "top-level startup signal: {} last-phase={} last-call={}",
                        error_msg, last_phase, last_call
                    );
                }
                let _ = super::builtins::dispatch_builtin(
                    self,
                    "message",
                    vec![Value::string(&error_msg)],
                );
                self.log_startup_state("top-level-signal");
                tracing::warn!("Top-level startup error: {}", error_msg);
                Ok(Value::NIL)
            }
            Err(flow) => Err(flow),
        }
    }

    fn trace_startup_state_enabled(&self) -> bool {
        std::env::var("NEOMACS_TRACE_STARTUP_STATE")
            .ok()
            .is_some_and(|value| value == "1")
    }

    fn log_startup_state(&self, phase: &str) {
        if !self.trace_startup_state_enabled() {
            return;
        }

        let current_buffer = self
            .buffers
            .current_buffer()
            .map(|buffer| buffer.name.clone())
            .unwrap_or_else(|| "<none>".to_string());
        let selected_frame = self.frames.selected_frame().map(|frame| {
            let selected_window_buffer = frame
                .selected_window()
                .and_then(|window| window.buffer_id())
                .and_then(|buffer_id| self.buffers.get(buffer_id))
                .map(|buffer| buffer.name.clone())
                .unwrap_or_else(|| "<missing>".to_string());
            format!(
                "id=0x{:x} size={}x{} selected-window=0x{:x} selected-window-buffer={}",
                frame.id.0,
                frame.width,
                frame.height,
                frame.selected_window.0,
                selected_window_buffer
            )
        });
        let frames = self
            .frames
            .frame_list()
            .into_iter()
            .map(|fid| format!("0x{:x}", fid.0))
            .collect::<Vec<_>>();

        tracing::info!(
            "startup-state phase={} command-line-args={} command-line-args-left={} command-line-processed={} window-system={} initial-window-system={} current-buffer={} selected-frame={:?} frames={:?}",
            phase,
            format_startup_value(self.obarray.symbol_value("command-line-args")),
            format_startup_value(self.obarray.symbol_value("command-line-args-left")),
            format_startup_value(self.obarray.symbol_value("command-line-processed")),
            format_startup_value(self.obarray.symbol_value("window-system")),
            format_startup_value(self.obarray.symbol_value("initial-window-system")),
            current_buffer,
            selected_frame,
            frames
        );
    }

    /// Command loop with error recovery.
    ///
    /// Mirrors GNU Emacs `command_loop_2()` (keyboard.c:1146).
    /// Wraps command_loop_1 with condition-case error handling.
    #[tracing::instrument(skip_all)]
    fn command_loop_2(&mut self) -> EvalResult {
        loop {
            match self.command_loop_1() {
                Ok(val) => return Ok(val),
                Err(flow @ Flow::Throw { .. }) => {
                    // Throws propagate (exit, top-level, etc.) without
                    // re-entering the command loop.  Re-running command_loop_1
                    // here traps minibuffer exit throws and blocks waiting for
                    // another key instead of unwinding like GNU Emacs.
                    return Err(flow);
                }
                Err(flow @ Flow::Signal(_))
                    if self
                        .command_loop
                        .keyboard
                        .kboard
                        .executing_kbd_macro
                        .is_some() =>
                {
                    return Err(flow);
                }
                Err(Flow::Signal(sig)) => {
                    // Error in command loop — display and restart.
                    // Mirrors cmd_error() in keyboard.c.
                    let sym_name = sig.symbol_name().to_string();
                    let data_str = sig
                        .data
                        .iter()
                        .map(|v| format!("{}", v))
                        .collect::<Vec<_>>()
                        .join(" ");

                    // Display the error in the echo area
                    let error_msg = if data_str.is_empty() {
                        sym_name.clone()
                    } else {
                        format!("{}: {}", sym_name, data_str)
                    };
                    let _ = super::builtins::dispatch_builtin(
                        self,
                        "message",
                        vec![Value::string(&error_msg)],
                    );
                    tracing::error!("Command loop error: {}", error_msg);

                    // Clear prefix arg on error (like GNU Emacs)
                    self.assign("prefix-arg", Value::NIL);

                    // Ring the bell for quit signals
                    if sym_name == "quit" {
                        let _ = super::builtins::dispatch_builtin(self, "ding", vec![]);
                    }

                    // Restart the command loop.
                    continue;
                }
            }
        }
    }

    /// Main command loop — read key sequence, look up binding, execute.
    ///
    /// Mirrors GNU Emacs `command_loop_1()` (keyboard.c:1306).
    /// This is the core interactive loop: read → dispatch → redisplay.
    #[tracing::instrument(skip_all)]
    fn command_loop_1(&mut self) -> EvalResult {
        loop {
            if !self.command_loop.running {
                return Ok(Value::NIL);
            }

            self.flush_pending_safe_funcalls();

            if self.executing_kbd_macro_iteration_complete_for_command_loop() {
                self.assign("this-command", Value::NIL);
                return Ok(Value::NIL);
            }

            // Transfer prefix-arg → current-prefix-arg before each command
            // (mirrors keyboard.c command_loop_1 logic).
            let prefix_arg = self.eval_symbol("prefix-arg").unwrap_or(Value::NIL);
            self.assign("current-prefix-arg", prefix_arg);
            self.assign("prefix-arg", Value::NIL);

            // Read a complete key sequence (may be multi-key, e.g. C-x C-f).
            let (keys, binding) = self.read_key_sequence()?;

            if binding.is_nil() {
                // Undefined key sequence — reset prefix arg
                self.assign("prefix-arg", Value::NIL);
                let desc: Vec<String> = keys.iter().map(|v| format!("{:?}", v)).collect();
                tracing::debug!("Undefined key sequence: {}", desc.join(" "));
                continue;
            }

            // Set this-command, last-command-event, this-command-keys
            self.assign("this-command", binding);
            if let Some(last) = keys.last() {
                self.assign("last-command-event", *last);
            }
            tracing::debug!(
                "command_loop_1: binding={} current_buffer={:?} active_minibuffer_window={:?}",
                self.this_command_name_for_log(),
                self.buffers.current_buffer_id(),
                self.active_minibuffer_window
            );

            // Run pre-command-hook
            let _ = self.run_hook_if_bound("pre-command-hook");

            // Execute the Lisp-owned command-execute function like GNU Emacs.
            let exec_result = self.apply(Value::symbol("command-execute"), vec![binding]);

            if let Err(ref flow) = exec_result {
                match flow {
                    Flow::Throw { .. } => return exec_result,
                    Flow::Signal(_)
                        if self
                            .command_loop
                            .keyboard
                            .kboard
                            .executing_kbd_macro
                            .is_some() =>
                    {
                        return exec_result;
                    }
                    Flow::Signal(sig) => {
                        // Log error but continue the loop
                        // (mirrors cmd_error in keyboard.c)
                        let data_strs: Vec<String> =
                            sig.data.iter().map(|v| format!("{}", v)).collect();
                        tracing::warn!(
                            "Command error: ({} [{}])",
                            sig.symbol_name(),
                            data_strs.join(", ")
                        );
                    }
                }
            }

            // Update last-command
            if let Ok(this_cmd) = self.eval_symbol("this-command") {
                self.assign("last-command", this_cmd);
            }

            // Run post-command-hook
            let _ = self.run_hook_if_bound("post-command-hook");
            let _ = self.update_active_region_selection_after_command();

            if exec_result.is_ok()
                && self.command_loop.keyboard.kboard.defining_kbd_macro
                && self
                    .eval_symbol("prefix-arg")
                    .unwrap_or(Value::NIL)
                    .is_nil()
            {
                self.finalize_kbd_macro_runtime_chars();
            }
        }
    }

    fn executing_kbd_macro_iteration_complete_for_command_loop(&self) -> bool {
        matches!(
            self.command_loop.keyboard.kboard.executing_kbd_macro.as_ref(),
            Some(events) if self.command_loop.keyboard.kboard.kbd_macro_index >= events.len()
        ) && self
            .command_loop
            .keyboard
            .kboard
            .unread_selection_event
            .is_none()
            && self.command_loop.keyboard.kboard.unread_events.is_empty()
    }

    pub(crate) fn execute_kbd_macro_iteration_via_command_loop(&mut self) -> EvalResult {
        let saved_running = self.command_loop.running;
        if !saved_running {
            self.command_loop.running = true;
        }
        self.assign("prefix-arg", Value::NIL);
        let result = self.command_loop_2();
        if !saved_running && self.command_loop.running {
            self.command_loop.running = false;
        }
        result
    }

    pub(crate) fn with_executing_kbd_macro_runtime<F>(
        &mut self,
        macro_events: Vec<Value>,
        run: F,
    ) -> EvalResult
    where
        F: FnOnce(&mut Self) -> EvalResult,
    {
        let scope = ExecutingKbdMacroRuntimeScope {
            snapshot: self.snapshot_executing_kbd_macro_runtime(),
            real_this_command: self.eval_symbol("real-this-command").unwrap_or(Value::NIL),
        };
        self.begin_executing_kbd_macro_runtime(macro_events);
        let result = run(self);
        let cleanup = self.finish_executing_kbd_macro_runtime_scope(scope);
        match cleanup {
            Ok(v) if v.is_nil() => result,
            Ok(other) => Ok(other),
            Err(flow) => Err(flow),
        }
    }

    pub(crate) fn reset_executing_kbd_macro_runtime_iteration(&mut self) {
        self.set_executing_kbd_macro_runtime_index(0);
    }

    fn finish_executing_kbd_macro_runtime_scope(
        &mut self,
        scope: ExecutingKbdMacroRuntimeScope,
    ) -> EvalResult {
        self.restore_executing_kbd_macro_runtime(scope.snapshot);
        self.assign("real-this-command", scope.real_this_command);
        self.run_hook_if_bound("kbd-macro-termination-hook")
    }

    fn pending_gnu_timer(timer: Value) -> Option<PendingGnuTimer> {
        if !timer.is_vector() {
            return None;
        };

        let slots = timer.as_vector_data()?.clone();
        if !(9..=10).contains(&slots.len()) {
            return None;
        }

        if !slots[0].is_nil() {
            return None;
        }

        if !slots[7].is_nil() {
            // Idle timers remain on the GNU Lisp path, but NeoVM still does
            // not track GUI/TTY idleness with GNU's fidelity yet. Avoid
            // conflating ordinary timer behavior with partial idle semantics.
            return None;
        }

        Some(PendingGnuTimer {
            timer,
            when: GnuTimerTimestamp {
                high_seconds: slots[1].as_int()?,
                low_seconds: slots[2].as_int()?,
                usecs: slots[3].as_int()?,
                psecs: slots.get(8).and_then(|v| v.as_int()).unwrap_or(0),
            },
        })
    }

    fn pending_gnu_idle_timer(timer: Value) -> Option<PendingGnuTimer> {
        if !timer.is_vector() {
            return None;
        };

        let slots = timer.as_vector_data()?.clone();
        if !(9..=10).contains(&slots.len()) {
            return None;
        }

        if !slots[0].is_nil() {
            return None;
        }

        if slots[7].is_nil() {
            return None;
        }

        Some(PendingGnuTimer {
            timer,
            when: GnuTimerTimestamp {
                high_seconds: slots[1].as_int()?,
                low_seconds: slots[2].as_int()?,
                usecs: slots[3].as_int()?,
                psecs: slots.get(8).and_then(|v| v.as_int()).unwrap_or(0),
            },
        })
    }

    /// Run a named hook if it is bound and non-nil.
    pub(crate) fn run_hook_if_bound(&mut self, hook_name: &str) -> EvalResult {
        match self.eval_symbol(hook_name) {
            Ok(hook_val) if !hook_val.is_nil() => {
                // (run-hooks 'HOOK)
                super::builtins::dispatch_builtin(self, "run-hooks", vec![Value::symbol(hook_name)])
                    .unwrap_or(Ok(Value::NIL))
            }
            _ => Ok(Value::NIL),
        }
    }

    pub(crate) fn queue_pending_safe_funcall(&mut self, function: Value, args: Vec<Value>) {
        self.pending_safe_funcalls
            .push(PendingSafeFuncall { function, args });
    }

    pub(crate) fn queue_pending_safe_hook(&mut self, hook_name: &str, args: &[Value]) {
        self.queue_pending_safe_funcall(
            Value::symbol("run-hook-with-args"),
            std::iter::once(Value::symbol(hook_name))
                .chain(args.iter().copied())
                .collect(),
        );
    }

    pub(crate) fn flush_pending_safe_funcalls(&mut self) {
        while let Some(funcall) = self.pending_safe_funcalls.pop() {
            let _ = self.apply(funcall.function, funcall.args);
        }
    }

    fn update_active_region_selection_after_command(&mut self) -> EvalResult {
        if self
            .eval_symbol("mark-active")
            .unwrap_or(Value::NIL)
            .is_nil()
        {
            return Ok(Value::NIL);
        }

        let transient_mark_mode = self
            .eval_symbol("transient-mark-mode")
            .unwrap_or(Value::NIL);
        if transient_mark_mode == Value::symbol("identity") {
            self.assign("transient-mark-mode", Value::NIL);
        } else if transient_mark_mode == Value::symbol("only") {
            self.assign("transient-mark-mode", Value::symbol("identity"));
        }

        if !self
            .eval_symbol("deactivate-mark")
            .unwrap_or(Value::NIL)
            .is_nil()
        {
            let _ = self.apply(Value::symbol("deactivate-mark"), vec![])?;
            self.assign("saved-region-selection", Value::NIL);
            return Ok(Value::NIL);
        }

        if self
            .apply(Value::symbol("display-selections-p"), vec![])?
            .is_nil()
        {
            self.assign("saved-region-selection", Value::NIL);
            return Ok(Value::NIL);
        }

        if self
            .eval_symbol("select-active-regions")
            .unwrap_or(Value::NIL)
            .is_nil()
        {
            self.assign("saved-region-selection", Value::NIL);
            return Ok(Value::NIL);
        }

        if self
            .apply(Value::symbol("region-active-p"), vec![])?
            .is_nil()
        {
            self.assign("saved-region-selection", Value::NIL);
            return Ok(Value::NIL);
        }

        let this_command = self.eval_symbol("this-command").unwrap_or(Value::NIL);
        let inhibited_commands = self
            .eval_symbol("selection-inhibit-update-commands")
            .unwrap_or(Value::NIL);
        if self
            .apply(
                Value::symbol("memq"),
                vec![this_command, inhibited_commands],
            )?
            .is_truthy()
        {
            self.assign("saved-region-selection", Value::NIL);
            return Ok(Value::NIL);
        }

        let region_extract = self
            .eval_symbol("region-extract-function")
            .unwrap_or(Value::symbol("buffer-substring"));
        let text = self.apply(region_extract, vec![Value::NIL])?;
        let text_len = match self.apply(Value::symbol("length"), vec![text])?.kind() {
            ValueKind::Fixnum(len) => len,
            _ => 0,
        };
        if text_len > 0 {
            let _ = self.apply(
                Value::symbol("gui-set-selection"),
                vec![Value::symbol("PRIMARY"), text],
            )?;
        }
        let _ = super::builtins::dispatch_builtin(
            self,
            "run-hook-with-args",
            vec![Value::symbol("post-select-region-hook"), text],
        )
        .unwrap_or(Ok(Value::NIL))?;
        self.assign("saved-region-selection", Value::NIL);
        Ok(Value::NIL)
    }

    /// Trigger redisplay — calls the layout engine and sends frame to render thread.
    ///
    /// Mirrors GNU Emacs `redisplay()` (dispnew.c:5259).
    /// In batch mode (no callback), this is a no-op.
    pub(crate) fn redisplay(&mut self) {
        self.sync_pending_resize_events();
        // Take the callback out to satisfy the borrow checker:
        // the callback receives &mut self, but we can't call a closure
        // stored in &mut self while &mut self is borrowed.
        if let Some(mut f) = self.redisplay_fn.take() {
            let saved = self.buffers.reset_outermost_restrictions();
            f(self);
            let _ = super::builtins::run_redisplay_window_change_hooks(self);
            self.buffers.restore_outermost_restrictions(saved);
            self.redisplay_fn = Some(f);
        } else {
            let _ = super::builtins::run_redisplay_window_change_hooks(self);
        }
    }

    fn this_command_name_for_log(&self) -> String {
        self.eval_symbol("this-command")
            .map(|value| format!("{}", value))
            .unwrap_or_else(|_| "<unbound>".to_string())
    }

    /// Perform a full mark-and-sweep garbage collection.
    #[tracing::instrument(level = "debug", skip(self))]
    pub fn gc_collect(&mut self) {
        // Clear source_literal_cache before GC — it uses *const Expr raw
        // pointers as keys which can alias after Rc<Vec<Expr>> bodies are
        // freed, causing ABA: a new lambda body at the same address gets
        // a stale cached Value from a collected lambda's expression.
        self.source_literal_cache.clear();
        self.macro_expansion_cache.clear();
        let roots = self.collect_roots();
        // Use the new tagged heap for GC collection
        self.tagged_heap.collect(roots.into_iter());
        self.gc_pending = false;
        self.gc_count += 1;
        self.run_post_gc_hook();
    }

    fn with_gc_inhibited<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.gc_inhibit_depth += 1;
        let result = f(self);
        self.gc_inhibit_depth -= 1;
        result
    }

    fn run_post_gc_hook(&mut self) {
        let hook = crate::emacs_core::hook_runtime::hook_symbol_by_name(self, "post-gc-hook");
        let _ = self.with_gc_inhibited(|eval| {
            crate::emacs_core::hook_runtime::safe_run_named_hook(eval, hook, &[])
        });
    }

    /// Number of gray objects to process per incremental marking step.
    const MARK_WORK_LIMIT: usize = 1024;

    /// Incremental GC safe point.
    ///
    /// In gc_stress mode, always does a full collection for maximum bug
    /// detection.  Otherwise, drives an incremental mark-sweep state machine:
    ///
    ///   Idle → (threshold?) → begin_marking → Marking
    ///   Marking → mark_some(LIMIT) → (done?) → sweep → Idle
    pub fn gc_safe_point(&mut self) {
        if self.gc_inhibit_depth > 0 {
            return;
        }
        // Always do full STW collection with conservative stack scan.
        // The incremental approach had correctness issues where objects
        // reachable only through the Rust call stack could be swept
        // if the marking phase didn't see them.
        if self.gc_pending || self.tagged_heap.should_collect() {
            self.gc_collect();
        }
    }

    /// GNU-style quit processing used from evaluator boundaries.
    ///
    /// Mirrors `process_quit_flag` in GNU `eval.c`: clear `quit-flag`, then
    /// honor `throw-on-input`, `kill-emacs`, or signal `quit`.
    fn process_quit_flag(&mut self) -> Result<(), Flow> {
        let flag = self
            .obarray
            .symbol_value_id(self.quit_flag_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        self.obarray
            .set_symbol_value_id(self.quit_flag_symbol, Value::NIL);

        let throw_on_input = self
            .obarray
            .symbol_value_id(self.throw_on_input_symbol)
            .copied()
            .unwrap_or(Value::NIL);

        if flag
            .as_symbol_id()
            .map_or(false, |sym| sym == self.kill_emacs_symbol)
        {
            self.request_shutdown(0, false);
            return Err(signal("quit", vec![]));
        }

        if !throw_on_input.is_nil() && equal_value(&flag, &throw_on_input, 0) {
            return Err(Flow::Throw {
                tag: throw_on_input,
                value: Value::T,
            });
        }

        Err(signal("quit", vec![]))
    }

    /// GNU `maybe_quit`: do nothing when `quit-flag` is nil or
    /// `inhibit-quit` is non-nil; otherwise process the quit request.
    fn maybe_quit(&mut self) -> Result<(), Flow> {
        let quit_flag = self
            .obarray
            .symbol_value_id(self.quit_flag_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if quit_flag.is_nil() {
            return Ok(());
        }

        let inhibit_quit = self
            .obarray
            .symbol_value_id(self.inhibit_quit_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if inhibit_quit.is_truthy() {
            return Ok(());
        }

        self.process_quit_flag()
    }

    pub(crate) fn quit_flag_value(&self) -> Value {
        self.obarray
            .symbol_value_id(self.quit_flag_symbol)
            .copied()
            .unwrap_or(Value::NIL)
    }

    pub(crate) fn set_quit_flag_value(&mut self, value: Value) {
        self.obarray
            .set_symbol_value_id(self.quit_flag_symbol, value);
    }

    pub(crate) fn quit_char(&self) -> i64 {
        self.quit_char
    }

    pub(crate) fn set_quit_char(&mut self, quit: i64) {
        self.quit_char = quit & 0o377;
    }

    pub(crate) fn event_is_quit_char(&self, event: &Value) -> bool {
        event
            .as_fixnum()
            .map_or(false, |code| code == self.quit_char)
    }

    pub(crate) fn request_quit_from_keyboard_input(&mut self) {
        if self.quit_flag_value().is_nil() {
            self.set_quit_flag_value(Value::T);
        }
    }

    pub(crate) fn clear_quit_flag_after_read_key_sequence_event(&mut self, event: &Value) {
        if !self.event_is_quit_char(event) {
            return;
        }

        let quit_flag = self.quit_flag_value();
        if quit_flag.is_nil() {
            return;
        }

        let throw_on_input = self
            .obarray
            .symbol_value_id(self.throw_on_input_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if equal_value(&quit_flag, &throw_on_input, 0) {
            return;
        }

        self.set_quit_flag_value(Value::NIL);
    }

    pub(crate) fn input_pending_p_filters_events(&self) -> bool {
        self.obarray
            .symbol_value("input-pending-p-filter-events")
            .copied()
            .unwrap_or(Value::T)
            .is_truthy()
    }

    pub(crate) fn track_mouse_enabled(&self) -> bool {
        self.obarray
            .symbol_value("track-mouse")
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy()
    }

    fn should_ignore_while_no_input_event(&self, event: &crate::keyboard::InputEvent) -> bool {
        let ignore_symbol = match event {
            crate::keyboard::InputEvent::Focus { focused, .. } => {
                Some(if *focused { "focus-in" } else { "focus-out" })
            }
            crate::keyboard::InputEvent::MonitorsChanged { .. } => Some("monitors-changed"),
            crate::keyboard::InputEvent::SelectWindow { .. } => Some("select-window"),
            _ => None,
        };
        let Some(ignore_symbol) = ignore_symbol else {
            return false;
        };

        let ignore_list = self
            .obarray
            .symbol_value("while-no-input-ignore-events")
            .copied()
            .unwrap_or(Value::NIL);
        super::value::list_to_vec(&ignore_list)
            .into_iter()
            .flatten()
            .any(|value| value.is_symbol_named(ignore_symbol))
    }

    pub(crate) fn input_event_counts_as_pending(
        &self,
        event: &crate::keyboard::InputEvent,
        filter_events: bool,
    ) -> bool {
        match event {
            crate::keyboard::InputEvent::Resize { .. } => false,
            crate::keyboard::InputEvent::Focus { .. } if !filter_events => false,
            crate::keyboard::InputEvent::MouseMove { .. } => self.track_mouse_enabled(),
            _ if filter_events && self.should_ignore_while_no_input_event(event) => false,
            _ => true,
        }
    }

    fn poll_pending_input_for_throw_on_input(&mut self) {
        self.sync_pending_resize_events();

        let throw_on_input = self
            .obarray
            .symbol_value_id(self.throw_on_input_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if throw_on_input.is_nil() {
            return;
        }

        let quit_flag = self
            .obarray
            .symbol_value_id(self.quit_flag_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if !quit_flag.is_nil() {
            return;
        }

        if self
            .command_loop
            .keyboard
            .pending_input_events
            .iter()
            .any(|event| self.input_event_counts_as_pending(event, true))
        {
            self.obarray
                .set_symbol_value_id(self.quit_flag_symbol, throw_on_input);
        }
    }

    /// Interrupt on input for GNU-style `throw-on-input` users such as
    /// `while-no-input`, while preserving the input event for later reads.
    pub(crate) fn interrupt_for_input_event_if_requested(
        &mut self,
        event: crate::keyboard::InputEvent,
    ) -> Result<bool, Flow> {
        let throw_on_input = self
            .obarray
            .symbol_value_id(self.throw_on_input_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if throw_on_input.is_nil() {
            return Ok(false);
        }

        let inhibit_quit = self
            .obarray
            .symbol_value_id(self.inhibit_quit_symbol)
            .copied()
            .unwrap_or(Value::NIL);
        if inhibit_quit.is_truthy() {
            return Ok(false);
        }

        self.command_loop
            .keyboard
            .pending_input_events
            .push_front(event);
        self.obarray
            .set_symbol_value_id(self.quit_flag_symbol, throw_on_input);
        self.maybe_quit()?;
        Ok(true)
    }

    /// Match GNU `eval_sub` / `funcall_general`: quit check first, then GC.
    fn maybe_gc_and_quit(&mut self) -> Result<(), Flow> {
        self.poll_pending_input_for_throw_on_input();
        self.maybe_quit()?;
        self.gc_safe_point();
        Ok(())
    }

    /// Save the current length of temp_roots for later restoration.
    pub(crate) fn save_temp_roots(&self) -> usize {
        self.temp_roots.len()
    }

    /// Add a value to temp_roots so it survives GC.
    pub(crate) fn push_temp_root(&mut self, val: Value) {
        self.temp_roots.push(val);
    }

    /// Restore temp_roots to a previously saved length.
    pub(crate) fn restore_temp_roots(&mut self, saved_len: usize) {
        self.temp_roots.truncate(saved_len);
    }

    // -----------------------------------------------------------------------
    // HandleScope — RAII formalization of the temp_roots save/restore pattern
    // -----------------------------------------------------------------------

    /// Execute `f` within a HandleScope. All Values rooted via
    /// `push_temp_root` during `f` are automatically unrooted when
    /// `f` returns (even on error/early-return).
    ///
    /// This is the RAII replacement for save/restore_temp_roots.
    /// Equivalent to:
    /// ```ignore
    /// let saved = ctx.save_temp_roots();
    /// let result = f(ctx);
    /// ctx.restore_temp_roots(saved);
    /// result
    /// ```
    #[inline]
    pub(crate) fn with_gc_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved_len = self.temp_roots.len();
        let result = f(self);
        self.temp_roots.truncate(saved_len);
        result
    }

    /// Like `with_gc_scope` but for fallible operations.
    /// Restores temp_roots even on error paths.
    #[inline]
    pub(crate) fn with_gc_scope_result<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        let saved_len = self.temp_roots.len();
        let result = f(self);
        self.temp_roots.truncate(saved_len);
        result
    }

    /// Root a Value so it survives GC until the enclosing scope ends.
    /// Returns the Value unchanged (for chaining).
    #[inline]
    pub(crate) fn root(&mut self, val: Value) -> Value {
        self.temp_roots.push(val);
        val
    }

    /// Open a HandleScope that can be passed between functions.
    /// The scope MUST be closed via `scope.close(ctx)` when done.
    /// Prefer `with_gc_scope` for self-contained blocks.
    #[inline]
    pub(crate) fn open_gc_scope(&self) -> HandleScope {
        HandleScope {
            saved_len: self.temp_roots.len(),
        }
    }
}

/// A transferable GC rooting scope.
///
/// Represents a region of `temp_roots` that will be truncated when
/// the scope is closed. Use `Context::open_gc_scope()` to create.
///
/// For self-contained blocks, prefer `Context::with_gc_scope()` which
/// handles cleanup automatically. Use `HandleScope` when the scope
/// must be returned to a caller (e.g., eval_args returns a scope
/// that the caller closes after using the args).
#[must_use = "HandleScope must be closed via .close(ctx) to restore temp_roots"]
pub(crate) struct HandleScope {
    saved_len: usize,
}

impl HandleScope {
    /// Close the scope, restoring temp_roots to the saved length.
    #[inline]
    pub fn close(self, ctx: &mut Context) {
        ctx.temp_roots.truncate(self.saved_len);
    }

    /// Get the saved length (for interop with raw save/restore).
    #[inline]
    pub fn saved_len(&self) -> usize {
        self.saved_len
    }
}

impl Context {
    /// Whether lexical-binding is currently enabled.
    pub fn lexical_binding(&self) -> bool {
        self.obarray
            .symbol_value("lexical-binding")
            .is_some_and(|v| v.is_truthy())
    }

    pub(crate) fn current_input_mode_tuple(&self) -> (bool, bool, bool, i64) {
        // Batch oracle compatibility: flow-control and meta are fixed to
        // nil/t respectively; quit-char remains mutable like GNU Emacs.
        (self.input_mode_interrupt, false, true, self.quit_char)
    }

    pub(crate) fn set_input_mode_interrupt(&mut self, interrupt: bool) {
        self.input_mode_interrupt = interrupt;
    }

    fn sync_keyboard_runtime_binding_by_id(&mut self, sym_id: SymId, value: Value) {
        if sym_id == intern("input-decode-map") {
            self.command_loop.keyboard.set_input_decode_map(value);
        } else if sym_id == intern("local-function-key-map") {
            self.command_loop.keyboard.set_local_function_key_map(value);
        }
    }

    pub(crate) fn sync_keyboard_runtime_from_obarray(&mut self) {
        let input_decode_map = self
            .obarray
            .symbol_value("input-decode-map")
            .copied()
            .unwrap_or(Value::NIL);
        let local_function_key_map = self
            .obarray
            .symbol_value("local-function-key-map")
            .copied()
            .unwrap_or(Value::NIL);
        self.command_loop
            .keyboard
            .set_terminal_translation_maps(input_decode_map, local_function_key_map);
    }

    pub(crate) fn waiting_for_user_input(&self) -> bool {
        self.waiting_for_user_input
    }

    pub(crate) fn set_waiting_for_user_input(&mut self, waiting: bool) {
        self.waiting_for_user_input = waiting;
    }

    pub(crate) fn has_input_receiver(&self) -> bool {
        self.input_rx.is_some()
    }

    pub(crate) fn pop_unread_command_event(&mut self) -> Option<Value> {
        let current = match self.eval_symbol("unread-command-events") {
            Ok(value) => value,
            Err(_) => Value::NIL,
        };
        match current.kind() {
            ValueKind::Cons => {
                let head = current.cons_car();
                let tail = current.cons_cdr();
                self.assign("unread-command-events", tail);
                self.record_input_event(head);
                Some(head)
            }
            _ => None,
        }
    }

    pub(crate) fn peek_unread_command_event(&self) -> Option<Value> {
        let current = match self.eval_symbol("unread-command-events") {
            Ok(value) => value,
            Err(_) => Value::NIL,
        };
        match current.kind() {
            ValueKind::Cons => Some(current.cons_car()),
            _ => None,
        }
    }

    /// Prepend an event to the `unread-command-events` list so that the next
    /// `read_char` / `pop_unread_command_event` will consume it first.
    pub(crate) fn push_unread_command_event(&mut self, event: Value) {
        let current = match self.eval_symbol("unread-command-events") {
            Ok(value) => value,
            Err(_) => Value::NIL,
        };
        let new_list = Value::cons(event, current);
        self.assign("unread-command-events", new_list);
    }

    pub(crate) fn replace_unread_command_event_with_singleton(&mut self, event: Value) {
        self.assign("unread-command-events", Value::list(vec![event]));
    }

    /// Enable or disable lexical binding.
    pub fn set_lexical_binding(&mut self, enabled: bool) {
        self.obarray
            .set_symbol_value("lexical-binding", Value::bool_val(enabled));
    }

    /// Reset transient evaluator state at a completed top-level boundary.
    ///
    /// GNU reaches interactive/runtime boundaries by unwinding dynamic state
    /// back to the top level, not by discarding the binding stack.  NeoVM's
    /// source bootstrap can transiently accumulate bindings, lexical
    /// environments, and catch state while loading `loadup.el` and early
    /// startup Lisp, but those structures must be unwound before the
    /// evaluator is reused.
    pub(crate) fn clear_top_level_eval_state(&mut self) {
        self.unbind_to(0);
        while let Some(saved) = self.saved_lexenvs.pop() {
            self.lexenv = saved;
        }
        self.lexenv = Value::NIL;
        self.temp_roots.clear();
        self.condition_stack.clear();
        self.depth = 0;
    }

    #[cfg(test)]
    pub(crate) fn top_level_eval_state_is_clean(&self) -> bool {
        self.specpdl.is_empty()
            && self.lexenv.is_nil()
            && self.saved_lexenvs.is_empty()
            && self.temp_roots.is_empty()
            && self.condition_stack.is_empty()
            && self.depth == 0
    }

    #[cfg(test)]
    pub(crate) fn condition_stack_depth_for_test(&self) -> usize {
        self.condition_stack.len()
    }

    pub(crate) fn set_interpreted_closure_filter_fn(&mut self, filter_fn: Option<Value>) {
        self.interpreted_closure_filter_fn = filter_fn;
        if filter_fn.is_none() {
            self.interpreted_closure_trim_cache.clear();
        }
    }

    /// Load a file, converting EvalError back to Flow for use in special forms.
    pub fn load_file_internal(&mut self, path: &std::path::Path) -> EvalResult {
        super::load::load_file(self, path).map_err(|e| match e {
            EvalError::Signal {
                symbol,
                data,
                raw_data,
            } => {
                if let Some(raw) = raw_data {
                    signal_with_data(resolve_sym(symbol), raw)
                } else {
                    signal(resolve_sym(symbol), data)
                }
            }
            EvalError::UncaughtThrow { tag, value } => signal("no-catch", vec![tag, value]),
        })
    }

    pub(crate) fn eval_value_with_lexical_arg(
        &mut self,
        form: Value,
        lexical_arg: Option<Value>,
    ) -> EvalResult {
        let state = begin_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            lexical_arg,
        )?;
        let result = self.eval_value(&form);
        finish_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            state,
        );
        result
    }

    pub(crate) fn eval_lambda_body(&mut self, body: &[Expr]) -> EvalResult {
        stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || {
            self.sf_progn(body)
        })
    }

    pub(crate) fn begin_lambda_call(
        &mut self,
        lambda: &LambdaData,
        args: &[Value],
        func_value: Value,
    ) -> Result<ActiveLambdaCallState, Flow> {
        begin_lambda_call_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            &mut self.temp_roots,
            lambda,
            args,
            func_value,
        )
    }

    pub(crate) fn finish_lambda_call(&mut self, state: ActiveLambdaCallState) {
        finish_lambda_call_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            &mut self.temp_roots,
            state,
        );
    }

    /// Keep the Lisp-visible `features` variable in sync with the evaluator's
    /// internal feature set.
    pub(crate) fn sync_features_variable(&mut self) {
        sync_features_variable_in_state(&mut self.obarray, &self.features);
    }

    pub(crate) fn refresh_features_from_variable(&mut self) {
        refresh_features_from_variable_in_state(&self.obarray, &mut self.features);
    }

    fn has_feature(&mut self, name: &str) -> bool {
        feature_present_in_state(&self.obarray, &mut self.features, name)
    }

    pub(crate) fn add_feature(&mut self, name: &str) {
        add_feature_in_state(&mut self.obarray, &mut self.features, name);
    }

    pub(crate) fn feature_present(&mut self, name: &str) -> bool {
        self.has_feature(name)
    }

    /// Remove a feature (used to undo temporary provides during bootstrap).
    pub(crate) fn remove_feature(&mut self, name: &str) {
        remove_feature_in_state(&mut self.obarray, &mut self.features, name);
    }

    /// Access the obarray (for builtins that need it).
    pub fn obarray(&self) -> &Obarray {
        &self.obarray
    }

    /// Access the obarray mutably.
    pub fn obarray_mut(&mut self) -> &mut Obarray {
        &mut self.obarray
    }

    /// Public read access to the buffer manager.
    pub fn buffer_manager(&self) -> &BufferManager {
        &self.buffers
    }

    /// Public mutable access to the buffer manager.
    pub fn buffer_manager_mut(&mut self) -> &mut BufferManager {
        &mut self.buffers
    }

    /// Public read access to the frame manager.
    pub fn frame_manager(&self) -> &FrameManager {
        &self.frames
    }

    /// Public mutable access to the frame manager.
    pub fn frame_manager_mut(&mut self) -> &mut FrameManager {
        &mut self.frames
    }

    pub fn current_message_text(&self) -> Option<&str> {
        self.current_message.as_deref()
    }

    pub fn set_current_message(&mut self, message: Option<String>) {
        self.current_message = message;
    }

    pub fn clear_current_message(&mut self) {
        if self.current_message.is_none() {
            return;
        }
        let hook =
            crate::emacs_core::hook_runtime::hook_symbol_by_name(self, "echo-area-clear-hook");
        let _ = crate::emacs_core::hook_runtime::safe_run_named_hook(self, hook, &[]);
        self.current_message = None;
    }

    pub(crate) fn current_message_slot(&mut self) -> &mut Option<String> {
        &mut self.current_message
    }

    pub(crate) fn sync_keyboard_terminal_owner(&mut self) {
        let terminal_id = self
            .frames
            .selected_frame()
            .map(|frame| frame.terminal_id)
            .unwrap_or(crate::emacs_core::terminal::pure::TERMINAL_ID);
        self.command_loop.keyboard.select_terminal(terminal_id);
    }

    pub(crate) fn sync_keyboard_terminal_owner_for_input_frame(&mut self, emacs_frame_id: u64) {
        let terminal_id = if emacs_frame_id == 0 {
            self.frames
                .selected_frame()
                .map(|frame| frame.terminal_id)
                .unwrap_or(crate::emacs_core::terminal::pure::TERMINAL_ID)
        } else {
            self.frames
                .get(crate::window::FrameId(emacs_frame_id))
                .map(|frame| frame.terminal_id)
                .unwrap_or_else(|| {
                    self.frames
                        .selected_frame()
                        .map(|frame| frame.terminal_id)
                        .unwrap_or(crate::emacs_core::terminal::pure::TERMINAL_ID)
                })
        };
        self.command_loop.keyboard.select_terminal(terminal_id);
    }

    /// Public read access to the face table.
    pub fn face_table(&self) -> &FaceTable {
        &self.face_table
    }

    /// Public mutable access to the face table.
    pub fn face_table_mut(&mut self) -> &mut FaceTable {
        &mut self.face_table
    }

    /// Set a face attribute and bump the change counter.
    /// This is the canonical way to modify face definitions at runtime.
    pub fn set_face_attribute(
        &mut self,
        face_name: &str,
        attr: &str,
        value: crate::face::FaceAttrValue,
    ) -> bool {
        let changed = self.face_table.set_attribute(face_name, attr, value);
        if changed {
            self.face_change_count += 1;
        }
        changed
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    pub fn eval_expr(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        self.with_gc_scope(|ctx| {
            let mut opaques = Vec::new();
            collect_opaque_values(expr, &mut opaques);
            for v in &opaques {
                ctx.push_temp_root(*v);
            }
            ctx.eval(expr).map_err(map_flow)
        })
    }

    /// Evaluate a Value as code (like Elisp's `eval`).
    /// Converts Value to Expr, then evaluates.  OpaqueValueRef entries
    /// are rooted by the OpaqueValuePool automatically.
    /// Evaluate a runtime Value form, matching GNU Emacs's `eval_sub` in eval.c.
    ///
    /// This is the Value-based evaluator that works directly on `Value` (Lisp_Object
    /// equivalent) WITHOUT converting to `Expr` first. This eliminates the
    /// `value_to_expr` round-trip that was the primary performance bottleneck
    /// in macro expansion (50-100x slowdown vs GNU).
    ///
    /// Dispatch order (matching GNU eval.c:2552-2766):
    /// 1. Symbol → lexenv lookup or symbol-value
    /// 2. Non-cons → self-evaluating (return as-is)
    /// 3. Cons → special form / macro / function call
    pub fn eval_sub(&mut self, form: Value) -> EvalResult {
        // 1. Symbol → variable lookup (GNU eval.c:2554-2562)
        if let Some(sym_id) = form.as_symbol_id() {
            return self.eval_symbol_by_id(sym_id);
        }

        // 2. Non-cons → self-evaluating (GNU eval.c:2564-2565)
        if !form.is_cons() {
            return Ok(form);
        }

        self.maybe_gc_and_quit()?;

        let original_fun = form.cons_car();
        let original_args = form.cons_cdr();

        // Resolve function (GNU eval.c:2600-2605)
        let sym_id = original_fun.as_symbol_id();
        let name = sym_id.map(|id| resolve_sym(id));

        // Check for special forms (GNU eval.c UNEVALLED subrs)
        if let Some(name) = name {
            if super::subr_info::is_special_form(name) {
                if let Some(func) = self.obarray.symbol_function(name) {
                    if func.is_subr() {
                        // Convert args to Expr for special form handling.
                        // This is a shallow conversion — only the immediate args,
                        // not the deep tree. Special forms handle their own
                        // sub-evaluation.
                        let args_exprs = value_list_to_exprs(&original_args);
                        if let Some(result) = self.try_special_form(name, &args_exprs) {
                            return result;
                        }
                    }
                }
            }
        }

        // Resolve function value
        let func = if let Some(sym_id) = sym_id {
            match self.obarray.symbol_function_id(sym_id) {
                Some(f) if !f.is_nil() => {
                    let mut f = *f;
                    // Follow symbol indirection (GNU eval.c:2604)
                    if let Some(alias_id) = f.as_symbol_id() {
                        if let Some(resolved) =
                            self.obarray.indirect_function(resolve_sym(alias_id))
                        {
                            f = resolved;
                        }
                    }
                    // Handle autoload
                    if super::autoload::is_autoload_value(&f) {
                        let _ = super::autoload::builtin_autoload_do_load(
                            self,
                            vec![f, Value::from_sym_id(sym_id), Value::NIL],
                        )?;
                        match self.obarray.symbol_function_id(sym_id) {
                            Some(reloaded) if !reloaded.is_nil() => *reloaded,
                            _ => {
                                return Err(signal(
                                    "void-function",
                                    vec![Value::from_sym_id(sym_id)],
                                ));
                            }
                        }
                    } else {
                        f
                    }
                }
                _ => return Err(signal("void-function", vec![Value::from_sym_id(sym_id)])),
            }
        } else if original_fun.is_cons() {
            // Car is a list — could be (lambda ...) form
            // Evaluate it to get the function
            self.eval_sub(original_fun)?
        } else {
            return Err(signal("invalid-function", vec![original_fun]));
        };

        // Check for macro (GNU eval.c:2730-2755)
        if func.is_macro() {
            let lambda_data = func.get_lambda_data().unwrap().clone();
            let arg_values = value_list_to_values(&original_args);
            let expanded = self.with_macro_expansion_scope(|eval| {
                eval.apply_lambda(&lambda_data, arg_values, func)
            })?;
            // Evaluate expansion DIRECTLY — no value_to_expr round-trip!
            return self.eval_sub(expanded);
        }
        if func.is_cons() && func.cons_car().is_symbol_named("macro") {
            // Cons-cell macro: (macro . fn) — GNU eval.c:2730
            let macro_fn = func.cons_cdr();

            // GNU eval.c:2737-2750: bind lexical-binding and macroexp--dynvars
            let saved_lexbind = self.obarray().symbol_value("lexical-binding").cloned();
            let lexbind_val = if self.lexenv.is_nil() {
                Value::NIL
            } else {
                Value::T
            };
            self.set_variable("lexical-binding", lexbind_val);

            let saved_dynvars = self.obarray().symbol_value("macroexp--dynvars").cloned();
            let mut dynvars = saved_dynvars.unwrap_or(Value::NIL);
            {
                let mut p = self.lexenv;
                while p.is_cons() {
                    let e = p.cons_car();
                    if e.is_symbol() {
                        dynvars = Value::cons(e, dynvars);
                    }
                    p = p.cons_cdr();
                }
            }
            self.set_variable("macroexp--dynvars", dynvars);

            // GNU eval.c:2752: exp = apply1(Fcdr(fun), original_args)
            let arg_values = value_list_to_values(&original_args);
            let expanded = self.apply(macro_fn, arg_values)?;

            // Restore bindings
            if let Some(v) = saved_lexbind {
                self.set_variable("lexical-binding", v);
            }
            if let Some(v) = saved_dynvars {
                self.set_variable("macroexp--dynvars", v);
            } else {
                self.set_variable("macroexp--dynvars", Value::NIL);
            }

            // GNU eval.c:2754: val = eval_sub(exp)
            return self.eval_sub(expanded);
        }

        // Regular function call: evaluate args, then apply
        // (GNU eval.c:2625-2715)
        let mut args = Vec::new();
        let mut cursor = original_args;
        while cursor.is_cons() {
            let arg_form = cursor.cons_car();
            let arg_val = self.eval_sub(arg_form)?;
            self.push_temp_root(arg_val);
            args.push(arg_val);
            cursor = cursor.cons_cdr();
        }

        self.apply(func, args)
    }

    /// Legacy eval_value: delegates to eval_sub.
    pub fn eval_value(&mut self, value: &Value) -> EvalResult {
        self.eval_sub(*value)
    }

    pub fn eval_forms(&mut self, forms: &[Expr]) -> Vec<Result<Value, EvalError>> {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        let saved_len = self.temp_roots.len();
        let mut results = Vec::with_capacity(forms.len());
        for form in forms {
            let result = self.eval_expr(form);
            // Root successful values so they survive GC triggered by later forms.
            if let Ok(ref val) = result {
                self.temp_roots.push(*val);
            }
            results.push(result);
        }
        self.temp_roots.truncate(saved_len);
        results
    }

    /// Set a global variable.
    pub fn set_variable(&mut self, name: &str, value: Value) {
        self.obarray.set_symbol_value(name, value);
    }

    pub(crate) fn sync_thread_runtime_bindings(&mut self) {
        if let Some(main_thread) = self.threads.thread_handle(0) {
            self.obarray.set_symbol_value("main-thread", main_thread);
        }
    }

    /// Set a function binding.
    pub fn set_function(&mut self, name: &str, value: Value) {
        self.obarray.set_symbol_function(name, value);
    }

    // -----------------------------------------------------------------------
    // Core eval
    // -----------------------------------------------------------------------

    pub(crate) fn eval(&mut self, expr: &Expr) -> EvalResult {
        // GNU Emacs only increments lisp_eval_depth for actual form evaluation
        // (lists = function calls / special forms), not for atoms (int, string,
        // symbol, etc.). This matches eval_sub in eval.c which increments before
        // the switch on form type, but atoms return immediately without recursing.
        let is_form = matches!(expr, Expr::List(_) | Expr::DottedList(_, _));
        if is_form {
            self.depth += 1;
        }
        // Sync max_depth from max-lisp-eval-depth variable only when we're
        // near the limit (avoids obarray lookup on every eval call).
        if self.depth > self.max_depth {
            if let Some(v) = self.obarray.symbol_value("max-lisp-eval-depth") {
                if let Some(n) = v.as_fixnum() {
                    let new_max = n.max(100) as usize;
                    if new_max != self.max_depth {
                        self.max_depth = new_max;
                    }
                }
            }
        }
        if self.depth > self.max_depth {
            let overflow_depth = self.depth as i64;
            self.depth -= 1;
            return Err(signal(
                "excessive-lisp-nesting",
                vec![Value::fixnum(overflow_depth)],
            ));
        }
        // Use stacker to dynamically grow the call stack when nearing
        // exhaustion.  The red-zone (256 KB) must be larger than the
        // combined stack frames between successive eval() calls (through
        // eval_list → apply → apply_lambda → bytecode VM).  When the
        // remaining stack falls below the red-zone a new segment is allocated
        // on the heap. GNU bootstrap/source-load recursion can legitimately
        // exceed a 2 MB segment long before max-lisp-eval-depth is reached.
        let result = match stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || {
            self.eval_inner(expr)
        }) {
            Err(Flow::Signal(sig)) => self
                .dispatch_signal_if_needed(sig)
                .map_or_else(|flow| Err(flow), |dispatched| Err(Flow::Signal(dispatched))),
            other => other,
        };
        if is_form {
            self.depth -= 1;
        }
        result
    }

    /// Evaluate a subform without incrementing depth. Used by special form
    /// handlers (if, let, while, progn, etc.) to match GNU Emacs behavior
    /// where special form body evaluation doesn't re-enter eval_sub.
    pub(crate) fn eval_subform(&mut self, expr: &Expr) -> EvalResult {
        stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || {
            self.eval_inner(expr)
        })
    }

    fn eval_inner(&mut self, expr: &Expr) -> EvalResult {
        if matches!(expr, Expr::List(_) | Expr::DottedList(_, _)) {
            self.maybe_gc_and_quit()?;
        }

        match expr {
            Expr::Int(v) => Ok(Value::fixnum(*v)),
            Expr::Float(v) => Ok(Value::make_float(*v)),
            Expr::ReaderLoadFileName => Ok(self
                .obarray
                .symbol_value("load-file-name")
                .cloned()
                .unwrap_or(Value::NIL)),
            Expr::Str(s) => Ok(Value::string(s.clone())),
            Expr::Char(c) => Ok(Value::char(*c)),
            Expr::Keyword(id) => Ok(Value::from_kw_id(*id)),
            Expr::Bool(true) => Ok(Value::T),
            Expr::Bool(false) => Ok(Value::NIL),
            Expr::Vector(items) => {
                // Emacs vector literals are self-evaluating constants; elements
                // are not evaluated in the current lexical/dynamic environment.
                // Use quote_to_runtime_value to resolve #$ (ReaderLoadFileName)
                // to the actual load-file-name string, matching GNU Emacs where
                // #$ is substituted at read time.
                let vals = items
                    .iter()
                    .map(|item| self.quote_to_runtime_value(item))
                    .collect();
                Ok(Value::vector(vals))
            }
            Expr::Symbol(id) => self.eval_symbol_by_id(*id),
            Expr::List(items) => self.eval_list(items),
            Expr::DottedList(items, last) => {
                // Evaluate as a list call, ignoring dotted cdr
                // (This is for `(func a b . rest)` style, which in practice
                //  means the dotted pair is rarely used in function calls)
                let _ = last;
                self.eval_list(items)
            }
            Expr::OpaqueValueRef(idx) => Ok(OPAQUE_POOL.with(|pool| pool.borrow().get(*idx))),
        }
    }

    /// Look up a symbol by its SymId. Uses the SymId directly for lexenv
    /// lookup (preserving uninterned symbol identity, like Emacs's EQ-based
    /// Fassq on Vinternal_interpreter_environment).
    pub(crate) fn eval_symbol_by_id(&self, sym_id: SymId) -> EvalResult {
        let symbol = resolve_sym(sym_id);
        let symbol_is_canonical =
            lookup_interned(symbol).is_some_and(|canonical| canonical == sym_id);
        // Keywords evaluate to themselves
        if symbol_is_canonical && symbol.starts_with(':') {
            return Ok(Value::from_kw_id(sym_id));
        }

        // GNU Emacs eval.c checks the lexenv for the ORIGINAL symbol
        // BEFORE resolving variable aliases.  This ensures that local
        // lexical bindings (let/lambda parameters) shadow variable aliases
        // even when the alias target is a special (dynamically scoped)
        // variable.  E.g. `argv` aliases to `command-line-args-left` (special),
        // but `(let ((argv (make-vector 10 nil))) argv)` must return the vector.
        // GNU `eval_sub` only checks the lexical alist here; bare-symbol
        // special declarations affect binding, not ordinary reads.
        if self.lexical_binding() && !is_runtime_dynamically_special(&self.obarray, sym_id) {
            if let Some(value) = lexenv_lookup(self.lexenv, sym_id) {
                return Ok(value);
            }
        }

        let resolved = super::builtins::resolve_variable_alias_id(self, sym_id)?;
        let resolved_name = resolve_sym(resolved);

        // Also check the lexenv for the resolved alias (rare but possible).
        if resolved != sym_id
            && self.lexical_binding()
            && !is_runtime_dynamically_special(&self.obarray, resolved)
        {
            if let Some(value) = lexenv_lookup(self.lexenv, resolved) {
                return Ok(value);
            }
        }

        // Dynamic scope lookup: specbind writes directly to obarray,
        // so for special variables just fall through to obarray lookup below.

        if symbol_is_canonical && symbol == "nil" {
            return Ok(Value::NIL);
        }
        if symbol_is_canonical && symbol == "t" {
            return Ok(Value::T);
        }

        let resolved_is_canonical =
            lookup_interned(resolved_name).is_some_and(|canonical| canonical == resolved);
        if resolved_is_canonical && resolved_name == "nil" {
            return Ok(Value::NIL);
        }
        if resolved_is_canonical && resolved_name == "t" {
            return Ok(Value::T);
        }
        if resolved_is_canonical && resolved_name.starts_with(':') {
            return Ok(Value::from_kw_id(resolved));
        }

        // Buffer-local bindings are name-based and must not intercept
        // uninterned symbols that merely share the same print name.
        if resolved_is_canonical && let Some(buf) = self.buffers.current_buffer() {
            if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
                return binding
                    .as_value()
                    .ok_or_else(|| signal("void-variable", vec![value_from_symbol_id(sym_id)]));
            }
        }

        // Obarray value cell
        if let Some(value) = self.obarray.symbol_value_id(resolved) {
            return Ok(*value);
        }

        Err(signal("void-variable", vec![value_from_symbol_id(sym_id)]))
    }

    pub(crate) fn eval_symbol(&self, symbol: &str) -> EvalResult {
        self.eval_symbol_by_id(intern(symbol))
    }

    fn apply_symbol_callable(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        if super::builtins::is_canonical_symbol_id(sym_id) {
            let name = resolve_sym(sym_id);
            let invalid_fn = if super::subr_info::is_special_form(name) {
                Value::subr(sym_id)
            } else {
                value_from_symbol_id(sym_id)
            };
            return self.apply_named_callable_by_id(
                sym_id,
                args,
                invalid_fn,
                rewrite_builtin_wrong_arity,
            );
        }

        if self.obarray.is_function_unbound_id(sym_id) {
            return Err(signal("void-function", vec![Value::from_sym_id(sym_id)]));
        }

        let Some(function) = self.obarray.symbol_function_id(sym_id).cloned() else {
            return Err(signal("void-function", vec![Value::from_sym_id(sym_id)]));
        };

        // Handle autoloads for non-canonical symbols the same as canonical
        // ones: trigger autoload-do-load before calling apply, so the raw
        // autoload cons never reaches apply_inner's Value::Cons path.
        if super::autoload::is_autoload_value(&function) {
            let name = resolve_sym(sym_id);
            return self.apply_named_autoload_callable(
                name,
                function,
                args,
                rewrite_builtin_wrong_arity,
            );
        }

        let function_is_callable = self.function_value_is_callable(&function);
        let result = self.apply_untraced(function, args);
        match &result {
            Err(Flow::Signal(sig))
                if sig.symbol_name() == "invalid-function" && !function_is_callable =>
            {
                Err(signal("invalid-function", vec![Value::from_sym_id(sym_id)]))
            }
            _ => result,
        }
    }

    fn apply_symbol_callable_untraced(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        if super::builtins::is_canonical_symbol_id(sym_id) {
            let name = resolve_sym(sym_id);
            let invalid_fn = if super::subr_info::is_special_form(name) {
                Value::subr(sym_id)
            } else {
                value_from_symbol_id(sym_id)
            };
            return self.apply_named_callable_by_id_core(
                sym_id,
                args,
                invalid_fn,
                rewrite_builtin_wrong_arity,
            );
        }

        if self.obarray.is_function_unbound_id(sym_id) {
            return Err(signal("void-function", vec![Value::from_sym_id(sym_id)]));
        }

        let Some(function) = self.obarray.symbol_function_id(sym_id).cloned() else {
            return Err(signal("void-function", vec![Value::from_sym_id(sym_id)]));
        };

        if super::autoload::is_autoload_value(&function) {
            let name = resolve_sym(sym_id);
            return self.apply_named_autoload_callable(
                name,
                function,
                args,
                rewrite_builtin_wrong_arity,
            );
        }

        let function_is_callable = self.function_value_is_callable(&function);
        let result = self.apply_untraced(function, args);
        match &result {
            Err(Flow::Signal(sig))
                if sig.symbol_name() == "invalid-function" && !function_is_callable =>
            {
                Err(signal("invalid-function", vec![Value::from_sym_id(sym_id)]))
            }
            _ => result,
        }
    }

    pub(crate) fn function_value_is_callable(&self, function: &Value) -> bool {
        match function.kind() {
            ValueKind::Veclike(VecLikeType::Lambda)
            | ValueKind::Veclike(VecLikeType::ByteCode)
            | ValueKind::Veclike(VecLikeType::Macro) => true,
            ValueKind::Veclike(VecLikeType::Subr) => {
                let bound_name = function.as_subr_id().unwrap();
                !super::subr_info::is_special_form(resolve_sym(bound_name))
            }
            ValueKind::Cons => {
                super::autoload::is_autoload_value(function)
                    || function.cons_car().is_symbol_named("lambda")
                    || function.cons_car().is_symbol_named("closure")
                    || function.cons_car().is_symbol_named("macro")
            }
            ValueKind::Symbol(id) => {
                super::builtins::symbols::resolve_indirect_symbol_by_id(self, id)
                    .is_some_and(|(_, resolved)| self.function_value_is_callable(&resolved))
            }
            _ => false,
        }
    }

    fn function_value_is_advice_wrapper(&self, function: &Value) -> bool {
        let advice_type = Some(Value::symbol("advice"));
        match function.kind() {
            ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
                function.get_lambda_data().map(|d| d.doc_form).flatten() == advice_type
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                function.get_bytecode_data().map(|d| d.doc_form).flatten() == advice_type
            }
            ValueKind::Symbol(id) => {
                super::builtins::symbols::resolve_indirect_symbol_by_id(self, id)
                    .is_some_and(|(_, resolved)| self.function_value_is_advice_wrapper(&resolved))
            }
            _ => false,
        }
    }

    fn advice_wrapper_frame_function(&self, function: Value) -> Value {
        if self.function_value_is_advice_wrapper(&function)
            && let Some(symbol) = self.advice_wrapper_symbol_alias(&function)
        {
            return Value::from_sym_id(symbol);
        }
        function
    }

    fn advice_wrapper_symbol_alias(&self, function: &Value) -> Option<SymId> {
        self.obarray.all_symbols().into_iter().find_map(|name| {
            let symbol = intern(name);
            self.obarray
                .symbol_function_id(symbol)
                .filter(|bound| eq_value(bound, function))
                .map(|_| symbol)
        })
    }

    /// Evaluate a slice of expressions into a Vec, rooting intermediate results
    /// in `temp_roots` so they survive any GC triggered by later evaluations.
    ///
    /// Returns `(args, scope)`.  The evaluated args remain rooted in
    /// `temp_roots` so that subsequent `apply` / `apply_named_callable`
    /// calls can't have their args freed by GC.  The caller **must** call
    /// `scope.close(self)` once the args are no longer needed (typically
    /// after `apply` returns).
    fn eval_args(&mut self, exprs: &[Expr]) -> Result<(Vec<Value>, HandleScope), Flow> {
        let scope = self.open_gc_scope();
        let mut args = Vec::with_capacity(exprs.len());
        // Save depth before evaluating arguments. GNU Emacs does not increment
        // lisp_eval_depth for argument evaluation — only for the function call
        // itself and for top-level form evaluation. Without this, a call like
        // (list e1 e2 ... e4000) consumes 4000 depth units in NeoVM but only 1
        // in GNU. This caused excessive-lisp-nesting errors during Doom Emacs
        // startup where large lists (load-path, auto-mode-alist) are constructed.
        let saved_depth = self.depth;
        for expr in exprs.iter() {
            match self.eval(expr) {
                Ok(val) => {
                    self.depth = saved_depth;
                    self.temp_roots.push(val);
                    args.push(val);
                }
                Err(Flow::Signal(sig))
                    if sig.symbol_name() == "wrong-type-argument"
                        && matches!(
                            expr,
                            Expr::List(items)
                                if matches!(
                                    items.first(),
                                    Some(Expr::Symbol(id))
                                        if resolve_sym(*id) == "lambda" || resolve_sym(*id) == "closure"
                                )
                        ) =>
                {
                    self.depth = saved_depth;
                    scope.close(self);
                    return Err(signal("invalid-function", vec![quote_to_value(expr)]));
                }
                Err(e) => {
                    self.depth = saved_depth;
                    scope.close(self);
                    return Err(e);
                }
            }
        }
        // Do NOT truncate — caller closes scope after apply.
        Ok((args, scope))
    }

    fn eval_list(&mut self, items: &[Expr]) -> EvalResult {
        let Some((head, tail)) = items.split_first() else {
            return Ok(Value::NIL);
        };

        if let Expr::Symbol(id) = head {
            let sym_id = *id;
            let name = resolve_sym(sym_id);

            // Reserved for evaluator-owned forms that must bypass macro
            // shadowing. The current source-compatible path keeps this empty.
            if super::subr_info::is_evaluator_sf_skip_macroexpand(name) {
                if let Some(func) = self.obarray.symbol_function(name) {
                    let is_macro = func.is_macro()
                        || (func.is_cons() && func.cons_car().is_symbol_named("macro"));
                    if is_macro {
                        if let Some(result) = self.try_special_form(name, tail) {
                            return result;
                        }
                    }
                }
            }

            // Check for macro expansion (from obarray function cell)
            if let Some(mut func) = self.obarray.symbol_function_id(sym_id).cloned() {
                if func.is_nil() {
                    return Err(signal("void-function", vec![Value::symbol(name)]));
                }

                // Follow symbol indirection chain to detect macros behind
                // defalias aliases (e.g. cl-incf -> incf where incf is a
                // macro).  Only replace `func` when the target is a macro
                // — non-macro aliases are handled by the apply path below.
                if let Some(alias_id) = func.as_symbol_id() {
                    if let Some(resolved) = self.obarray.indirect_function(resolve_sym(alias_id)) {
                        let is_macro = resolved.is_macro()
                            || (resolved.is_cons() && resolved.cons_car().is_symbol_named("macro"));
                        if is_macro {
                            func = resolved;
                        }
                    }
                }

                if super::autoload::is_autoload_value(&func) {
                    // GNU eval.c handles macro autoloads before argument
                    // evaluation: load the file only if the autoload TYPE is
                    // macro-like, then retry the normal macro-expansion path
                    // with the freshly installed definition.
                    let _ = super::autoload::builtin_autoload_do_load(
                        self,
                        vec![func, Value::symbol(name), Value::symbol("macro")],
                    )?;
                    if let Some(loaded_macro) = self.obarray.symbol_function_id(sym_id).cloned() {
                        let is_loaded_macro = loaded_macro.is_macro()
                            || (loaded_macro.is_cons()
                                && loaded_macro.cons_car().is_symbol_named("macro"));
                        if is_loaded_macro {
                            func = loaded_macro;
                        }
                    }
                }

                if func.is_macro() {
                    // GNU eval.c approach: call macro function with
                    // unevaluated args, then eval_sub the result directly.
                    // No value_to_expr round-trip.
                    let lambda_data = func.get_lambda_data().unwrap().clone();

                    // Bind lexical-binding during expansion (GNU eval.c:2737)
                    let saved_lexbind = self.obarray().symbol_value("lexical-binding").cloned();
                    let lexbind_val = if self.lexenv.is_nil() {
                        Value::NIL
                    } else {
                        Value::T
                    };
                    self.set_variable("lexical-binding", lexbind_val);

                    // Propagate macroexp--dynvars (GNU eval.c:2741-2750)
                    let saved_dynvars = self.obarray().symbol_value("macroexp--dynvars").cloned();
                    let mut dynvars = saved_dynvars.unwrap_or(Value::NIL);
                    {
                        let mut p = self.lexenv;
                        while p.is_cons() {
                            let e = p.cons_car();
                            if e.is_symbol() {
                                dynvars = Value::cons(e, dynvars);
                            }
                            p = p.cons_cdr();
                        }
                    }
                    self.set_variable("macroexp--dynvars", dynvars);

                    // Convert Expr args to Values for the macro call
                    let arg_values: Vec<Value> = tail.iter().map(quote_to_value).collect();
                    for v in &arg_values {
                        self.push_temp_root(*v);
                    }

                    let expanded_value = self.with_macro_expansion_scope(|eval| {
                        eval.apply_lambda(&lambda_data, arg_values, func)
                    })?;

                    // Restore bindings
                    if let Some(v) = saved_lexbind {
                        self.set_variable("lexical-binding", v);
                    }
                    if let Some(v) = saved_dynvars {
                        self.set_variable("macroexp--dynvars", v);
                    } else {
                        self.set_variable("macroexp--dynvars", Value::NIL);
                    }

                    // eval_sub directly — no value_to_expr round-trip
                    return self.eval_sub(expanded_value);
                }
                // Handle cons-cell macros: (macro . fn) — used by byte-run.el's
                // (defalias 'defmacro (cons 'macro #'(lambda ...)))
                // Matches GNU eval.c lines 2730-2755: bind lexical-binding
                // and macroexp--dynvars during expansion.
                if func.is_cons() {
                    let car = func.cons_car();
                    if car.is_symbol_named("macro") {
                        let cache_key = (
                            func.bits(),
                            tail.as_ptr() as usize,
                            self.macro_expansion_context_key(),
                        );
                        let current_fp = tail_fingerprint(tail);
                        if !self.macro_cache_disabled {
                            if let Some(cached) =
                                self.macro_expansion_cache.get(&cache_key).cloned()
                            {
                                if cached.fingerprint == current_fp {
                                    self.macro_cache_hits += 1;
                                    let expanded = cached.expanded.clone();
                                    return self.eval(&expanded);
                                }
                                // Fingerprint mismatch → ABA detected, fall through to re-expand
                            }
                        }

                        let expand_start = std::time::Instant::now();
                        let expanded_value = {
                            let scope = self.open_gc_scope();
                            let macro_fn = func.cons_cdr();
                            self.push_temp_root(macro_fn);

                            // GNU eval.c: bind lexical-binding during macro expansion
                            let lexbind_val = if self.lexenv.is_nil() {
                                Value::NIL
                            } else {
                                Value::T
                            };
                            let saved_lexbind =
                                self.obarray().symbol_value("lexical-binding").cloned();
                            self.set_variable("lexical-binding", lexbind_val);

                            // GNU eval.c: propagate macroexp--dynvars from lexenv
                            let saved_dynvars =
                                self.obarray().symbol_value("macroexp--dynvars").cloned();
                            let mut dynvars = saved_dynvars.unwrap_or(Value::NIL);
                            {
                                let mut p = self.lexenv;
                                while p.is_cons() {
                                    let e = p.cons_car();
                                    if e.is_symbol() {
                                        dynvars = Value::cons(e, dynvars);
                                    }
                                    p = p.cons_cdr();
                                }
                            }
                            self.set_variable("macroexp--dynvars", dynvars);

                            // Root all arg values during macro expansion to survive GC.
                            let arg_values: Vec<Value> = tail.iter().map(quote_to_value).collect();
                            for v in &arg_values {
                                self.push_temp_root(*v);
                            }
                            let expanded_value = self.with_macro_expansion_scope(|eval| {
                                eval.apply(macro_fn, arg_values)
                            })?;

                            // Restore bindings
                            if let Some(v) = saved_lexbind {
                                self.set_variable("lexical-binding", v);
                            }
                            if let Some(v) = saved_dynvars {
                                self.set_variable("macroexp--dynvars", v);
                            } else {
                                self.set_variable("macroexp--dynvars", Value::NIL);
                            }

                            scope.close(self);
                            expanded_value
                        };

                        let expand_elapsed = expand_start.elapsed();
                        self.macro_cache_misses += 1;
                        self.macro_expand_total_us += expand_elapsed.as_micros() as u64;
                        if expand_elapsed.as_millis() > 50 {
                            tracing::debug!("cons-macro expand took {expand_elapsed:.2?}");
                        }

                        // Evaluate expansion DIRECTLY via eval_sub — no
                        // value_to_expr round-trip. Matches GNU eval.c:2754.
                        return self.eval_sub(expanded_value);
                    }
                }

                if let Some(bound_name) = func.as_subr_id() {
                    if resolve_sym(bound_name) == name && super::subr_info::is_special_form(name) {
                        if let Some(result) = self.try_special_form(name, tail) {
                            return result;
                        }
                    }
                }

                // Explicit function-cell bindings override special-form fallback.
                let (args, scope) = self.eval_args(tail)?;
                if super::autoload::is_autoload_value(&func) {
                    let writeback_args = args.clone();
                    let result =
                        self.apply_named_callable_by_id(sym_id, args, Value::subr(sym_id), false);
                    scope.close(self);
                    if let Ok(value) = &result {
                        self.maybe_writeback_mutating_first_arg(name, None, &writeback_args, value);
                    }
                    return result;
                }
                let writeback_args = args.clone();
                let result =
                    self.apply_named_callable_by_id(sym_id, args, Value::subr(sym_id), false);
                scope.close(self);
                if let Ok(value) = &result {
                    self.maybe_writeback_mutating_first_arg(name, None, &writeback_args, value);
                }
                return result;
            }

            // Evaluator-only forms like `lambda` still need a fallback path
            // because they are not public GNU subrs. Public special forms are
            // materialized into the function cell during init and should not be
            // recreated here.
            if !self.obarray.is_function_unbound_id(sym_id)
                && !super::subr_info::is_special_form(name)
            {
                if let Some(result) = self.try_special_form(name, tail) {
                    return result;
                }
            }

            match self.resolve_named_call_target_by_id(sym_id) {
                NamedCallTarget::Void => {
                    return Err(signal("void-function", vec![Value::symbol(name)]));
                }
                NamedCallTarget::SpecialForm => {
                    return Err(signal("invalid-function", vec![Value::subr(sym_id)]));
                }
                _ => {}
            }

            // Regular function call — GNU resolves the callee first. A
            // void/invalid function symbol must signal before any argument
            // forms are evaluated.
            let (args, scope) = self.eval_args(tail)?;

            let writeback_args = args.clone();
            let result = self.apply_named_callable_by_id(sym_id, args, Value::subr(sym_id), false);
            scope.close(self);
            if let Ok(value) = &result {
                self.maybe_writeback_mutating_first_arg(name, None, &writeback_args, value);
            }
            return result;
        }

        // Head is a list (possibly a lambda expression)
        if let Expr::List(lambda_form) = head {
            if let Some(Expr::Symbol(id)) = lambda_form.first() {
                if resolve_sym(*id) == "lambda" {
                    let func = self.eval_lambda(&lambda_form[1..])?;
                    self.push_temp_root(func);
                    let (args, scope) = self.eval_args(tail)?;
                    let result = self.apply(func, args);
                    scope.close(self);
                    self.temp_roots.pop();
                    return result;
                }
            }
        }

        // Head is an opaque callable value (Lambda, ByteCode, Subr, etc.)
        // embedded in code via value_to_expr (e.g., from eval/macro expansion).
        if let Expr::OpaqueValueRef(idx) = head {
            let func = OPAQUE_POOL.with(|pool| pool.borrow().get(*idx));
            self.push_temp_root(func);
            let (args, scope) = self.eval_args(tail)?;
            let result = self.apply(func, args);
            scope.close(self);
            self.temp_roots.pop();
            return result;
        }

        Err(signal("invalid-function", vec![quote_to_value(head)]))
    }

    fn maybe_writeback_mutating_first_arg(
        &mut self,
        called_name: &str,
        alias_target: Option<&str>,
        call_args: &[Value],
        result: &Value,
    ) {
        let mutates_fillarray =
            called_name == "fillarray" || alias_target.is_some_and(|name| name == "fillarray");
        let mutates_aset = called_name == "aset" || alias_target.is_some_and(|name| name == "aset");
        if !mutates_fillarray && !mutates_aset {
            return;
        }
        let Some(first_arg) = call_args.first() else {
            return;
        };
        if !first_arg.is_string() {
            return;
        }

        let replacement = if mutates_fillarray {
            if !result.is_string() || eq_value(first_arg, result) {
                return;
            }
            *result
        } else {
            if call_args.len() < 3 {
                return;
            }
            let Ok(updated) =
                super::builtins::aset_string_replacement(first_arg, &call_args[1], &call_args[2])
            else {
                return;
            };
            if eq_value(first_arg, &updated) {
                return;
            }
            updated
        };

        if first_arg.as_str() == replacement.as_str() {
            return;
        }

        let mut visited = HashSet::new();
        // Walk the lexenv cons alist and replace alias refs in binding values
        {
            let mut lexenv_val = self.lexenv;
            Self::replace_alias_refs_in_value(
                &mut lexenv_val,
                first_arg,
                &replacement,
                &mut visited,
            );
            self.lexenv = lexenv_val;
        }
        // Dynamic bindings are now in the obarray (via specbind), so
        // the obarray iteration below handles them.
        if let Some(current_id) = self.buffers.current_buffer_id()
            && let Some(buf) = self.buffers.get_mut(current_id)
        {
            for value in buf.bound_buffer_local_values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }

        let symbols: Vec<String> = self
            .obarray
            .all_symbols()
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in symbols {
            if let Some(symbol) = self.obarray.get_mut(&name) {
                match &mut symbol.value {
                    super::symbol::SymbolValue::Plain(Some(value)) => {
                        Self::replace_alias_refs_in_value(
                            value,
                            first_arg,
                            &replacement,
                            &mut visited,
                        );
                    }
                    super::symbol::SymbolValue::BufferLocal {
                        default: Some(value),
                        ..
                    } => {
                        Self::replace_alias_refs_in_value(
                            value,
                            first_arg,
                            &replacement,
                            &mut visited,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    fn replace_alias_refs_in_value(
        value: &mut Value,
        from: &Value,
        to: &Value,
        visited: &mut HashSet<usize>,
    ) {
        if eq_value(value, from) {
            *value = *to;
            return;
        }

        match value.kind() {
            ValueKind::Cons => {
                let key = value.bits() ^ 0x1;
                if !visited.insert(key) {
                    return;
                }
                let mut new_car = value.cons_car();
                let mut new_cdr = value.cons_cdr();
                Self::replace_alias_refs_in_value(&mut new_car, from, to, visited);
                Self::replace_alias_refs_in_value(&mut new_cdr, from, to, visited);
                value.set_car(new_car);
                value.set_cdr(new_cdr);
            }
            ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
                let key = value.bits() ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut values = value.as_vector_data().unwrap().clone();
                for item in values.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                *value.as_vector_data_mut().unwrap() = values;
            }
            ValueKind::Veclike(VecLikeType::HashTable) => {
                let key = value.bits() ^ 0x4;
                if !visited.insert(key) {
                    return;
                }
                let mut ht = value.as_hash_table().unwrap().clone();
                let old_ptr = if from.is_string() {
                    Some(from.bits())
                } else {
                    None
                };
                let new_ptr = if to.is_string() {
                    Some(to.bits())
                } else {
                    None
                };
                if matches!(ht.test, HashTableTest::Eq | HashTableTest::Eql) {
                    if let (Some(old_ptr), Some(new_ptr)) = (old_ptr, new_ptr) {
                        if let Some(existing) = ht.data.remove(&HashKey::Ptr(old_ptr)) {
                            ht.data.insert(HashKey::Ptr(new_ptr), existing);
                        }
                        if ht.key_snapshots.remove(&HashKey::Ptr(old_ptr)).is_some() {
                            ht.key_snapshots.insert(HashKey::Ptr(new_ptr), *to);
                        }
                        for k in &mut ht.insertion_order {
                            if *k == HashKey::Ptr(old_ptr) {
                                *k = HashKey::Ptr(new_ptr);
                            }
                        }
                    }
                }
                for item in ht.data.values_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                *value.as_hash_table_mut().unwrap() = ht;
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Special forms
    // -----------------------------------------------------------------------

    fn try_special_form(&mut self, name: &str, tail: &[Expr]) -> Option<EvalResult> {
        // GNU Emacs handles special forms inline in eval_sub without
        // re-incrementing lisp_eval_depth for each subform. Save and
        // restore depth so special form body evaluation doesn't
        // accumulate depth beyond the initial call's increment.
        let saved_depth = self.depth;
        let result = self.try_special_form_inner(name, tail);
        self.depth = saved_depth;
        result
    }

    fn try_special_form_inner(&mut self, name: &str, tail: &[Expr]) -> Option<EvalResult> {
        Some(match name {
            // ---- GNU Emacs C special forms (eval.c UNEVALLED) ----
            "quote" => self.sf_quote(tail),
            "function" => self.sf_function(tail),
            "let" => self.sf_let(tail),
            "let*" => self.sf_let_star(tail),
            "setq" => self.sf_setq(tail),
            "if" => self.sf_if(tail),
            "and" => self.sf_and(tail),
            "or" => self.sf_or(tail),
            "cond" => self.sf_cond(tail),
            "while" => self.sf_while(tail),
            "progn" => self.sf_progn(tail),
            "prog1" => self.sf_prog1(tail),
            "defvar" => self.sf_defvar(tail),
            "defconst" => self.sf_defconst(tail),
            "catch" => self.sf_catch(tail),
            "unwind-protect" => self.sf_unwind_protect(tail),
            "condition-case" => self.sf_condition_case(tail),
            // ---- GNU Emacs C special forms (editfns.c) ----
            "save-excursion" => self.sf_save_excursion(tail),
            "save-current-buffer" => super::misc::sf_save_current_buffer(self, tail),
            "save-restriction" => self.sf_save_restriction(tail),
            // ---- GNU Emacs C special form stub (callint.c) ----
            "interactive" => Ok(Value::NIL),
            // ---- Context-internal (not a special form in GNU) ----
            "lambda" => self.eval_lambda(tail),
            // ---- NeoVM-specific ----
            "byte-code-literal" => self.sf_byte_code_literal(tail),
            "byte-code" => self.sf_byte_code(tail),
            _ => return None,
        })
    }

    fn sf_quote(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("quote"), Value::fixnum(tail.len() as i64)],
            ));
        }
        Ok(self.quote_to_runtime_value(&tail[0]))
    }

    fn sf_function(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("function"), Value::fixnum(tail.len() as i64)],
            ));
        }
        match &tail[0] {
            Expr::List(items) => {
                // #'(lambda ...) — create closure
                if let Some(Expr::Symbol(id)) = items.first() {
                    if resolve_sym(*id) == "lambda" {
                        return self.eval_lambda(&items[1..]);
                    }
                }
                Ok(self.quote_to_runtime_value(&tail[0]))
            }
            _ => Ok(self.quote_to_runtime_value(&tail[0])),
        }
    }

    fn sf_let(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("let"), Value::fixnum(tail.len() as i64)],
            ));
        }

        let mut lexical_bindings: Vec<(SymId, Value)> = Vec::new();
        let mut dynamic_sym_ids: Vec<(SymId, Value)> = Vec::new();
        let use_lexical = self.lexical_binding();
        let mut constant_binding_error: Option<String> = None;

        // Root binding values during evaluation so GC triggered by later
        // initializers doesn't collect earlier ones.
        let saved_roots = self.temp_roots.len();
        match &tail[0] {
            Expr::List(entries) => {
                for binding in entries {
                    match binding {
                        Expr::Symbol(id) => {
                            if let Some(name) = symbol_sets_constant_error(*id) {
                                if constant_binding_error.is_none() {
                                    constant_binding_error = Some(name.to_owned());
                                }
                                continue;
                            }
                            if use_lexical
                                && !self.obarray.is_special_id(*id)
                                && !lexenv_declares_special(self.lexenv, *id)
                            {
                                lexical_bindings.push((*id, Value::NIL));
                            } else {
                                dynamic_sym_ids.push((*id, Value::NIL));
                            }
                        }
                        Expr::List(pair) if !pair.is_empty() => {
                            let Expr::Symbol(id) = &pair[0] else {
                                self.temp_roots.truncate(saved_roots);
                                return Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("symbolp"), quote_to_value(&pair[0])],
                                ));
                            };
                            let value = if pair.len() > 1 {
                                match self.eval(&pair[1]) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        self.temp_roots.truncate(saved_roots);
                                        return Err(e);
                                    }
                                }
                            } else {
                                Value::NIL
                            };
                            self.temp_roots.push(value);
                            if let Some(name) = symbol_sets_constant_error(*id) {
                                if constant_binding_error.is_none() {
                                    constant_binding_error = Some(name.to_owned());
                                }
                                continue;
                            }
                            if use_lexical
                                && !self.obarray.is_special_id(*id)
                                && !lexenv_declares_special(self.lexenv, *id)
                            {
                                lexical_bindings.push((*id, value));
                            } else {
                                dynamic_sym_ids.push((*id, value));
                            }
                        }
                        _ => {
                            self.temp_roots.truncate(saved_roots);
                            return Err(signal("wrong-type-argument", vec![]));
                        }
                    }
                }
            }
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => {} // (let nil ...)
            Expr::DottedList(_, last) => {
                self.temp_roots.truncate(saved_roots);
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(last.as_ref())],
                ));
            }
            other => {
                self.temp_roots.truncate(saved_roots);
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(other)],
                ));
            }
        }
        // Binding values are about to be moved into dynamic/lexenv (rooted).
        self.temp_roots.truncate(saved_roots);
        if let Some(name) = constant_binding_error {
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        // Save specpdl depth for unbind_to.
        let specpdl_count = self.specpdl.len();

        let pushed_lex = !lexical_bindings.is_empty();
        // Save lexenv before prepending bindings.
        let saved_lexenv = if pushed_lex {
            let saved = self.lexenv;
            self.saved_lexenvs.push(saved);
            // Prepend each binding in source order — this matches GNU Emacs's
            // Flet which prepends (cons-es) each binding onto the alist,
            // naturally reversing the source order.
            for (sym_id, val) in &lexical_bindings {
                self.lexenv = lexenv_prepend(self.lexenv, *sym_id, *val);
            }
            true
        } else {
            false
        };
        // Use specbind for dynamic bindings (writes directly to obarray).
        for (sym_id, value) in &dynamic_sym_ids {
            self.specbind(*sym_id, *value);
        }

        let result = self.sf_progn(&tail[1..]);
        self.unbind_to(specpdl_count);
        if saved_lexenv {
            self.lexenv = self.saved_lexenvs.pop().unwrap();
        }

        result
    }

    fn sf_let_star(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("let*"), Value::fixnum(tail.len() as i64)],
            ));
        }

        let entries = match &tail[0] {
            Expr::List(entries) => entries.clone(),
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => Vec::new(), // &tail[0] gives &&Expr; id is &SymId
            Expr::DottedList(_, last) => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(last.as_ref())],
                ));
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(other)],
                ));
            }
        };

        let use_lexical = self.lexical_binding();

        // Save specpdl depth for unbind_to.
        let specpdl_count = self.specpdl.len();
        let saved_lexenv = if use_lexical {
            let saved = self.lexenv;
            self.saved_lexenvs.push(saved);
            true
        } else {
            false
        };

        let init_result: Result<(), Flow> = (|| {
            for binding in &entries {
                match binding {
                    Expr::Symbol(id) => {
                        if let Some(name) = symbol_sets_constant_error(*id) {
                            return Err(signal("setting-constant", vec![Value::symbol(name)]));
                        }
                        if use_lexical
                            && !self.obarray.is_special_id(*id)
                            && !lexenv_declares_special(self.lexenv, *id)
                        {
                            self.bind_lexical_value_rooted(*id, Value::NIL);
                        } else {
                            self.specbind(*id, Value::NIL);
                        }
                    }
                    Expr::List(pair) if !pair.is_empty() => {
                        let Expr::Symbol(id) = &pair[0] else {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("symbolp"), quote_to_value(&pair[0])],
                            ));
                        };
                        let value = if pair.len() > 1 {
                            self.eval(&pair[1])?
                        } else {
                            Value::NIL
                        };
                        if let Some(name) = symbol_sets_constant_error(*id) {
                            return Err(signal("setting-constant", vec![Value::symbol(name)]));
                        }
                        if use_lexical
                            && !self.obarray.is_special_id(*id)
                            && !lexenv_declares_special(self.lexenv, *id)
                        {
                            self.bind_lexical_value_rooted(*id, value);
                        } else {
                            self.specbind(*id, value);
                        }
                    }
                    _ => return Err(signal("wrong-type-argument", vec![])),
                }
            }
            Ok(())
        })();
        if let Err(error) = init_result {
            if saved_lexenv {
                self.lexenv = self.saved_lexenvs.pop().unwrap();
            }
            self.unbind_to(specpdl_count);
            return Err(error);
        }

        let result = self.sf_progn(&tail[1..]);
        self.unbind_to(specpdl_count);
        if saved_lexenv {
            self.lexenv = self.saved_lexenvs.pop().unwrap();
        }

        result
    }

    fn sf_setq(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Ok(Value::NIL);
        }
        if !tail.len().is_multiple_of(2) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("setq"), Value::fixnum(tail.len() as i64)],
            ));
        }

        let mut last = Value::NIL;
        let mut i = 0;
        while i < tail.len() {
            let (sym_id, name) = match &tail[i] {
                Expr::Symbol(id) => (*id, resolve_sym(*id)), // &tail[i] match
                Expr::Keyword(id) => (*id, resolve_sym(*id)), // &tail[i] match
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), quote_to_value(&tail[i])],
                    ));
                }
            };
            let value = self.eval(&tail[i + 1])?;
            let resolved = super::builtins::resolve_variable_alias_name(self, name)?;
            let resolved_id = intern(&resolved);
            if self.obarray.is_constant_id(resolved_id)
                && !self.has_local_binding_by_id(sym_id)
                && (resolved_id == sym_id || !self.has_local_binding_by_id(resolved_id))
            {
                return Err(signal("setting-constant", vec![Value::symbol(name)]));
            }
            // If the variable has an alias, use the resolved (interned) name.
            // Otherwise, preserve the original SymId for uninterned symbol support.
            if resolved != name {
                self.assign_with_watchers(&resolved, value, "set")?;
            } else {
                self.assign_with_watchers_by_id(sym_id, value, "set")?;
            }
            last = value;
            i += 2;
        }
        Ok(last)
    }

    fn sf_if(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("if"), Value::fixnum(tail.len() as i64)],
            ));
        }
        let cond = self.eval(&tail[0])?;
        if cond.is_truthy() {
            self.eval(&tail[1])
        } else {
            self.sf_progn(&tail[2..])
        }
    }

    fn sf_and(&mut self, tail: &[Expr]) -> EvalResult {
        let mut last = Value::T;
        for expr in tail {
            last = self.eval(expr)?;
            if last.is_nil() {
                return Ok(Value::NIL);
            }
        }
        Ok(last)
    }

    fn sf_or(&mut self, tail: &[Expr]) -> EvalResult {
        for expr in tail {
            let val = self.eval(expr)?;
            if val.is_truthy() {
                return Ok(val);
            }
        }
        Ok(Value::NIL)
    }

    fn sf_cond(&mut self, tail: &[Expr]) -> EvalResult {
        for clause in tail {
            let Expr::List(items) = clause else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(clause)],
                ));
            };
            if items.is_empty() {
                continue;
            }
            let test = self.eval(&items[0])?;
            if test.is_truthy() {
                if items.len() == 1 {
                    return Ok(test);
                }
                return self.sf_progn(&items[1..]);
            }
        }
        Ok(Value::NIL)
    }

    fn sf_while(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("while"), Value::fixnum(tail.len() as i64)],
            ));
        }
        let mut iters: u64 = 0;
        loop {
            let cond = self.eval(&tail[0])?;
            if cond.is_nil() {
                return Ok(Value::NIL);
            }
            self.sf_progn(&tail[1..])?;
            iters += 1;
            if iters == 1_000_000 {
                let cond_str = super::expr::print_expr(&tail[0]);
                tracing::warn!(
                    "while loop exceeded 1M iterations, cond: {}",
                    &cond_str[..cond_str.len().min(300)]
                );
            }
            self.maybe_quit()?;
        }
    }

    pub(crate) fn sf_progn(&mut self, forms: &[Expr]) -> EvalResult {
        let mut last = Value::NIL;
        for form in forms {
            last = self.eval(form)?;
        }
        Ok(last)
    }

    fn sf_prog1(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("prog1"), Value::fixnum(tail.len() as i64)],
            ));
        }
        let first = self.eval(&tail[0])?;
        let scope = self.open_gc_scope();
        self.push_temp_root(first);
        for form in &tail[1..] {
            if let Err(err) = self.eval(form) {
                scope.close(self);
                return Err(err);
            }
        }
        scope.close(self);
        Ok(first)
    }

    fn sf_defvar(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("defvar"), Value::fixnum(tail.len() as i64)],
            ));
        }
        if tail.len() > 3 {
            return Err(signal("error", vec![Value::string("Too many arguments")]));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        // Only set value if INITVALUE is provided and symbol is not already bound.
        // (defvar x) without INITVALUE only marks as special, does NOT bind.
        if tail.len() > 1 {
            if !self.obarray.boundp_id(*id) {
                let value = self.eval(&tail[1])?;
                self.obarray.set_symbol_value_id(*id, value);
            }
            self.obarray.make_special_id(*id);
        } else if self.lexical_binding()
            && !self.lexenv.is_nil()
            && !self.obarray.is_special_id(*id)
        {
            // Mirror GNU eval.c: simple `(defvar foo)` inside a lexical scope
            // only declares `foo` dynamically within the current lexical env.
            self.lexenv = Value::cons(Value::from_sym_id(*id), self.lexenv);
        }
        Ok(value_from_symbol_id(*id))
    }

    fn sf_defconst(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("defconst"), Value::fixnum(tail.len() as i64)],
            ));
        }
        if tail.len() > 3 {
            return Err(signal("error", vec![Value::string("Too many arguments")]));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        let name = resolve_sym(*id);
        let value = self.eval(&tail[1])?;
        // GNU Emacs defconst-1 path:
        // 1) define variable metadata, 2) set default value, 3) mark risky local.
        // It does NOT mark SYMBOL as a hard constant (no SYMBOL_NOWRITE).
        super::custom::builtin_set_default(self, vec![Value::symbol(name), value])?;
        self.obarray.make_special(name);
        self.obarray
            .put_property(name, "risky-local-variable", Value::T);
        Ok(Value::symbol(name))
    }

    /// Validate a `Flow::Throw` against the active catch stack.
    /// If a matching catch exists, pass through.  If not, convert to
    /// `Flow::Signal("no-catch", ...)` — mirrors GNU Emacs `Fthrow`.
    fn validate_throw(&self, flow: Flow) -> Flow {
        match flow {
            Flow::Throw { ref tag, ref value } => {
                if self.has_active_catch(tag) {
                    flow
                } else {
                    signal("no-catch", vec![*tag, *value])
                }
            }
            other => other,
        }
    }

    fn sf_catch(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("catch"), Value::fixnum(tail.len() as i64)],
            ));
        }
        let tag = self.eval(&tail[0])?;
        // Root tag so GC during body can't collect it.
        self.temp_roots.push(tag);
        self.push_condition_frame(ConditionFrame::Catch {
            tag,
            resume: ResumeTarget::InterpreterCatch,
        });
        let result = match self.sf_progn(&tail[1..]) {
            Ok(value) => Ok(value),
            Err(Flow::Throw {
                tag: thrown_tag,
                value,
            }) if eq_value(&tag, &thrown_tag) => Ok(value),
            Err(flow) => Err(flow),
        };
        self.pop_condition_frame();
        self.temp_roots.pop();
        result
    }

    fn sf_unwind_protect(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("unwind-protect"),
                    Value::fixnum(tail.len() as i64),
                ],
            ));
        }
        let primary = self.eval(&tail[0]);
        // Root the primary result so GC during cleanup can't collect it.
        // This includes values inside Flow::Signal / Flow::Throw data,
        // which live only on the Rust stack and are invisible to the GC
        // root scanner.
        let scope = self.open_gc_scope();
        match &primary {
            Ok(val) => {
                self.push_temp_root(*val);
            }
            Err(Flow::Signal(sig)) => {
                for v in &sig.data {
                    self.push_temp_root(*v);
                }
                if let Some(raw) = &sig.raw_data {
                    self.push_temp_root(*raw);
                }
            }
            Err(Flow::Throw { tag, value }) => {
                self.push_temp_root(*tag);
                self.push_temp_root(*value);
            }
        }
        let cleanup = self.sf_progn(&tail[1..]);
        scope.close(self);
        match cleanup {
            Ok(_) => primary,
            Err(flow) => Err(flow),
        }
    }

    fn sf_condition_case(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("condition-case"),
                    Value::fixnum(tail.len() as i64),
                ],
            ));
        }

        let var = match &tail[0] {
            Expr::Symbol(id) => *id,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), quote_to_value(other)],
                ));
            }
        };
        let body = &tail[1];
        let handlers = &tail[2..];

        // Emacs validates handler shape even when BODY exits normally.
        // Also extract the special :success handler (GNU eval.c:1587).
        let mut success_handler_idx: Option<usize> = None;
        for (i, handler) in handlers.iter().enumerate() {
            match handler {
                Expr::List(items) if !items.is_empty() => {
                    if let Expr::Keyword(kw) = &items[0] {
                        if resolve_sym(*kw) == ":success" {
                            success_handler_idx = Some(i);
                        }
                    }
                }
                Expr::List(_) => {}
                Expr::Symbol(id) if resolve_sym(*id) == "nil" => {}
                _ => {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!(
                            "Invalid condition handler: {}",
                            super::expr::print_expr(handler)
                        ))],
                    ));
                }
            }
        }

        let condition_stack_base = self.condition_stack_len();
        for (i, handler) in handlers.iter().enumerate().rev() {
            if success_handler_idx == Some(i) {
                continue;
            }
            if matches!(handler, Expr::Symbol(id) if resolve_sym(*id) == "nil") {
                continue;
            }
            let Expr::List(handler_items) = handler else {
                continue;
            };
            if handler_items.is_empty() {
                continue;
            }
            let conditions = self.quote_to_runtime_value(&handler_items[0]);
            self.push_condition_frame(ConditionFrame::ConditionCase {
                conditions,
                resume: ResumeTarget::InterpreterConditionCase {
                    handler_index: i,
                    condition_stack_base,
                },
            });
        }

        match self.eval(body) {
            Ok(value) => {
                self.truncate_condition_stack(condition_stack_base);
                // GNU eval.c:1618 — if there's a :success handler, bind VAR
                // to the body's return value and evaluate the handler body.
                if let Some(idx) = success_handler_idx {
                    if let Expr::List(items) = &handlers[idx] {
                        let bind_var = resolve_sym(var) != "nil";
                        let specpdl_count = self.specpdl.len();
                        if bind_var {
                            self.specbind(var, value);
                        }
                        let mut result = Value::NIL;
                        for form in &items[1..] {
                            result = self.eval(form)?;
                        }
                        self.unbind_to(specpdl_count);
                        return Ok(result);
                    }
                }
                Ok(value)
            }
            Err(Flow::Signal(sig)) => {
                self.truncate_condition_stack(condition_stack_base);
                if let Some(ResumeTarget::InterpreterConditionCase {
                    handler_index,
                    condition_stack_base: selected_stack_base,
                }) = sig.selected_resume.clone()
                    && selected_stack_base == condition_stack_base
                {
                    let handler = &handlers[handler_index];
                    let Expr::List(handler_items) = handler else {
                        return Err(signal("wrong-type-argument", vec![]));
                    };

                    let bind_var = resolve_sym(var) != "nil";
                    let binding_value = make_signal_binding_value(&sig);
                    let use_lexical_binding = bind_var
                        && self.lexical_binding()
                        && !is_runtime_dynamically_special(&self.obarray, var)
                        && !lexenv_declares_special(self.lexenv, var);

                    let specpdl_count = self.specpdl.len();
                    let pushed_lexenv = if use_lexical_binding {
                        let saved = self.lexenv;
                        self.saved_lexenvs.push(saved);
                        self.bind_lexical_value_rooted(var, binding_value);
                        true
                    } else {
                        if bind_var {
                            self.specbind(var, binding_value);
                        }
                        false
                    };
                    let result = self.sf_progn(&handler_items[1..]);
                    self.unbind_to(specpdl_count);
                    if pushed_lexenv {
                        self.lexenv = self.saved_lexenvs.pop().unwrap();
                    }
                    return result;
                }
                Err(Flow::Signal(sig))
            }
            // Flow::Throw bypasses condition-case entirely (GNU Emacs semantics).
            // The throw was already validated against the active catch stack when
            // it was created in sf_throw / builtin_throw. If there's no matching
            // catch, sf_throw signals no-catch as a Flow::Signal, which is
            // handled above.
            Err(flow @ Flow::Throw { .. }) => {
                self.truncate_condition_stack(condition_stack_base);
                Err(flow)
            }
        }
    }

    /// Convert an `Expr` to a `Value`, treating everything as literal data
    /// except `(byte-code-literal ...)` forms which are evaluated to produce
    /// `Value::ByteCode`. This is needed because `.elc` constant vectors
    /// contain literal values (lists, symbols, etc.) that must NOT be evaluated,
    /// but may also contain nested `#[...]` compiled functions (parsed as
    /// `(byte-code-literal VECTOR)`) that DO need evaluation.
    fn quote_to_value_with_bytecode(&mut self, expr: &Expr) -> EvalResult {
        match expr {
            Expr::List(elts)
                if matches!(
                    elts.first(),
                    Some(Expr::Symbol(s)) if *s == intern("byte-code-literal")
                ) =>
            {
                self.eval(expr)
            }
            Expr::Vector(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.quote_to_value_with_bytecode(item)?);
                }
                Ok(Value::vector(values))
            }
            _ => Ok(self.quote_to_runtime_value(expr)),
        }
    }

    pub(crate) fn reify_byte_code_literals(&mut self, expr: &Expr) -> Result<Expr, Flow> {
        match expr {
            Expr::List(elts)
                if matches!(
                    elts.first(),
                    Some(Expr::Symbol(s)) if *s == intern("byte-code-literal")
                ) =>
            {
                let val = self.sf_byte_code_literal(&elts[1..])?;
                Ok(Expr::OpaqueValueRef(
                    OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(val)),
                ))
            }
            Expr::List(items) => Ok(Expr::List(
                items
                    .iter()
                    .map(|item| self.reify_byte_code_literals(item))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::DottedList(items, tail) => Ok(Expr::DottedList(
                items
                    .iter()
                    .map(|item| self.reify_byte_code_literals(item))
                    .collect::<Result<Vec<_>, _>>()?,
                Box::new(self.reify_byte_code_literals(tail)?),
            )),
            Expr::Vector(items) => Ok(Expr::Vector(
                items
                    .iter()
                    .map(|item| self.reify_byte_code_literals(item))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            _ => Ok(expr.clone()),
        }
    }

    pub(crate) fn quote_to_runtime_value_in_state(obarray: &Obarray, expr: &Expr) -> Value {
        match expr {
            Expr::ReaderLoadFileName => obarray
                .symbol_value("load-file-name")
                .cloned()
                .unwrap_or(Value::NIL),
            Expr::List(items) => {
                let quoted = items
                    .iter()
                    .map(|item| Self::quote_to_runtime_value_in_state(obarray, item))
                    .collect::<Vec<_>>();
                Value::list(quoted)
            }
            Expr::DottedList(items, last) => {
                let head_vals: Vec<Value> = items
                    .iter()
                    .map(|item| Self::quote_to_runtime_value_in_state(obarray, item))
                    .collect();
                let tail_val = Self::quote_to_runtime_value_in_state(obarray, last);
                head_vals
                    .into_iter()
                    .rev()
                    .fold(tail_val, |acc, item| Value::cons(item, acc))
            }
            Expr::Vector(items) => {
                let vals = items
                    .iter()
                    .map(|item| Self::quote_to_runtime_value_in_state(obarray, item))
                    .collect();
                Value::vector(vals)
            }
            _ => quote_to_value(expr),
        }
    }

    pub(crate) fn quote_to_runtime_value(&mut self, expr: &Expr) -> Value {
        Self::quote_to_runtime_value_in_state(&self.obarray, expr)
    }

    fn sf_byte_code_literal(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("byte-code-literal"),
                    Value::fixnum(tail.len() as i64),
                ],
            ));
        }

        let Expr::Vector(items) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("vectorp"), quote_to_value(&tail[0])],
            ));
        };

        // Need at least 4 elements: [arglist bytecodes constants maxdepth ...]
        if items.len() < 4 {
            // Not a valid bytecode object; return as a plain vector.
            let values = items.iter().map(quote_to_value).collect::<Vec<_>>();
            return Ok(Value::vector(values));
        }

        // Convert each element to a Value. Constants are literal data,
        // except nested #[...] (byte-code-literal) forms that need evaluation.
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.quote_to_value_with_bytecode(item)?);
        }

        // Delegate to the shared make-byte-code construction.
        crate::emacs_core::builtins::make_byte_code_from_parts(
            &values[0],
            &values[1],
            &values[2],
            &values[3],
            values.get(4),
            values.get(5),
        )
    }

    /// Top-level `(byte-code "bytecodes" [constants] maxdepth)` form used in `.elc` files.
    /// Creates a temporary zero-arg ByteCodeFunction and executes it via the VM.
    fn sf_byte_code(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("byte-code"), Value::fixnum(tail.len() as i64)],
            ));
        }
        let trace_toplevel_bytecode = std::env::var_os("NEOVM_TRACE_TOPLEVEL_BYTECODE").is_some();
        let load_file_name = if trace_toplevel_bytecode {
            self.obarray()
                .symbol_value("load-file-name")
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            String::new()
        };
        let decode_start = trace_toplevel_bytecode.then(std::time::Instant::now);

        // The bytecode string and maxdepth are simple literals — quote them.
        // The constants vector may contain nested byte-code-literal forms.
        let bytecode_str = quote_to_value(&tail[0]);
        let constants_vec = self.quote_to_value_with_bytecode(&tail[1])?;
        let maxdepth = quote_to_value(&tail[2]);

        // Build a temporary zero-arg ByteCodeFunction
        use crate::emacs_core::bytecode::ByteCodeFunction;
        use crate::emacs_core::bytecode::decode::{
            decode_gnu_bytecode_with_offset_map, string_value_to_bytes,
        };
        use crate::emacs_core::value::LambdaParams;

        let raw_bytes = if let Some(s) = bytecode_str.as_str() {
            string_value_to_bytes(s)
        } else {
            Vec::new()
        };

        let mut constants: Vec<Value> = match constants_vec.kind() {
            ValueKind::Veclike(VecLikeType::Vector) => {
                constants_vec.as_vector_data().unwrap().clone()
            }
            _ => Vec::new(),
        };

        // Reify nested compiled literals in constants.
        for i in 0..constants.len() {
            constants[i] =
                crate::emacs_core::builtins::try_convert_nested_compiled_literal(constants[i]);
        }

        let (ops, gnu_byte_offset_map) =
            decode_gnu_bytecode_with_offset_map(&raw_bytes, &mut constants).map_err(|e| {
                signal(
                    "error",
                    vec![Value::string(format!("bytecode decode error: {}", e))],
                )
            })?;
        if let Some(start) = decode_start {
            tracing::info!(
                "TOPLEVEL-BYTECODE decode file={} bytes={} consts={} ops={} elapsed={:.2?}",
                load_file_name,
                raw_bytes.len(),
                constants.len(),
                ops.len(),
                start.elapsed()
            );
        }

        let max_stack = match maxdepth.kind() {
            ValueKind::Fixnum(n) => n as u16,
            _ => 16,
        };

        let bc = ByteCodeFunction {
            ops,
            constants,
            max_stack,
            params: LambdaParams::simple(vec![]),
            lexical: false,
            env: None,
            gnu_byte_offset_map: Some(gnu_byte_offset_map),
            docstring: None,
            doc_form: None,
            interactive: None,
        };

        // Execute via VM
        self.refresh_features_from_variable();
        let mut vm = super::bytecode::Vm::from_context(self);
        let exec_start = trace_toplevel_bytecode.then(std::time::Instant::now);
        let result = vm.execute(&bc, vec![]);
        if let Some(start) = exec_start {
            tracing::info!(
                "TOPLEVEL-BYTECODE exec   file={} ops={} elapsed={:.2?}",
                load_file_name,
                bc.ops.len(),
                start.elapsed()
            );
        }
        self.sync_features_variable();
        result
    }

    pub(crate) fn defalias_value(&mut self, sym: Value, def: Value) -> EvalResult {
        let plan = builtins::plan_defalias_in_obarray(self.obarray(), &[sym, def])?;
        let builtins::DefaliasPlan { action, result, .. } = plan;
        match action {
            builtins::DefaliasAction::SetFunction { symbol, definition } => {
                self.obarray.set_symbol_function_id(symbol, definition);
            }
            builtins::DefaliasAction::CallHook {
                hook,
                symbol_value,
                definition,
            } => {
                self.apply(hook, vec![symbol_value, definition])?;
            }
        }
        Ok(result)
    }

    #[tracing::instrument(level = "info", skip(self, subfeatures))]
    pub(crate) fn provide_value(
        &mut self,
        feature: Value,
        subfeatures: Option<Value>,
    ) -> EvalResult {
        provide_value_in_state(&mut self.obarray, &mut self.features, feature, subfeatures)?;
        // GNU Emacs Fprovide (fns.c): after adding the feature, run any
        // load-hooks registered in `after-load-alist`.
        //   tem = Fassq(feature, Vafter_load_alist);
        //   if (CONSP(tem))  Fmapc(Qfuncall, XCDR(tem));
        //
        // GNU Emacs Fprovide: (mapc #'funcall (cdr (assq feature after-load-alist)))
        // Does NOT clear load-file-name — the delayed-func from eval-after-load
        // defers to after-load-functions when load-file-name is set, and
        // do-after-load-evaluation fires those hooks after the file finishes loading.
        self.run_after_load_hooks_for_feature(feature)?;
        Ok(feature)
    }

    /// Run `after-load-alist` callbacks for FEATURE, mirroring GNU's
    /// `Fprovide` behavior: `(mapc #'funcall (cdr (assq feature after-load-alist)))`.
    fn run_after_load_hooks_for_feature(&mut self, feature: Value) -> Result<(), Flow> {
        let after_load_alist = self
            .obarray
            .symbol_value("after-load-alist")
            .cloned()
            .unwrap_or(Value::NIL);
        if after_load_alist.is_nil() {
            return Ok(());
        }
        // Walk after-load-alist looking for an entry whose car `eq` FEATURE.
        let entry = {
            let mut cursor = after_load_alist;
            let mut found = Value::NIL;
            while cursor.is_cons() {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if pair_car.is_cons() {
                    let inner_pair_car = pair_car.cons_car();
                    if inner_pair_car == feature {
                        found = pair_car;
                        break;
                    }
                }
                cursor = pair_cdr;
            }
            found
        };
        if entry.is_nil() {
            return Ok(());
        }
        // entry is (FEATURE callback1 callback2 ...).
        // Call funcall on each callback in the cdr.
        let callbacks = entry.cons_cdr();
        let mut cursor = callbacks;
        while cursor.is_cons() {
            let pair_car = cursor.cons_car();
            let pair_cdr = cursor.cons_cdr();
            let callback = pair_car;
            self.apply(callback, vec![])?;
            cursor = pair_cdr;
        }
        Ok(())
    }

    #[tracing::instrument(level = "info", skip(self), err(Debug))]
    pub(crate) fn require_value(
        &mut self,
        feature: Value,
        filename: Option<Value>,
        noerror: Option<Value>,
    ) -> EvalResult {
        let feature_name = match feature.kind() {
            ValueKind::Symbol(sid) => Some(resolve_sym(sid).to_string()),
            _ => None,
        };
        let filename_str = filename
            .as_ref()
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        match plan_require_in_state(
            &self.obarray,
            &mut self.features,
            &self.require_stack,
            feature,
            filename.clone(),
            noerror.clone(),
        ) {
            Err(e) => {
                tracing::error!(
                    feature = ?feature_name,
                    filename = ?filename_str,
                    "require plan failed: {:?}", e
                );
                return Err(e);
            }
            Ok(plan) => match plan {
                RequirePlan::Return(value) => Ok(value),
                RequirePlan::Load { sym_id, name, path } => {
                    self.require_stack.push(sym_id);
                    let result = (|| -> EvalResult {
                        self.load_file_internal(&path)?;
                        self.refresh_features_from_variable();
                        finish_require_in_state(&self.features, sym_id, &name)
                    })();
                    let _ = self.require_stack.pop();
                    if let Err(ref e) = result {
                        let noerror_val = noerror.as_ref().map(|v| !v.is_nil()).unwrap_or(false);
                        let path_str = path.display().to_string();
                        tracing::error!(
                            feature_name = ?feature_name,
                            path = %path_str,
                            noerror = noerror_val,
                            "require failed: {:?}", e
                        );
                    }
                    result
                }
            },
        }
    }

    fn sf_save_excursion(&mut self, tail: &[Expr]) -> EvalResult {
        let saved_buf = self.buffers.current_buffer().map(|b| b.id);
        let saved_marker = saved_buf.and_then(|buf_id| {
            let point = self.buffers.get(buf_id).map(|buf| buf.pt)?;
            Some(
                self.buffers
                    .create_marker(buf_id, point, InsertionType::Before),
            )
        });
        let result = self.sf_progn(tail);
        if let Some(buf_id) = saved_buf {
            self.restore_current_buffer_if_live(buf_id);
            if let Some(marker_id) = saved_marker {
                if let Some(saved_pt) = self.buffers.marker_position(buf_id, marker_id) {
                    let _ = self.buffers.goto_buffer_byte(buf_id, saved_pt);
                }
                self.buffers.remove_marker(marker_id);
            }
        }
        result
    }

    fn sf_save_restriction(&mut self, tail: &[Expr]) -> EvalResult {
        let saved = self.buffers.save_current_restriction_state();
        let saved_roots_len = self.temp_roots.len();
        if let Some(saved) = &saved {
            saved.trace_roots(&mut self.temp_roots);
        }
        let result = self.sf_progn(tail);
        if let Some(saved) = saved {
            self.buffers.restore_saved_restriction_state(saved);
        }
        self.temp_roots.truncate(saved_roots_len);
        result
    }

    // -----------------------------------------------------------------------
    // Lambda / Function application
    // -----------------------------------------------------------------------

    fn maybe_use_cached_interpreted_closure_filter(
        &mut self,
        closure_hook: Value,
        params_expr: &Expr,
        body_exprs: &[Expr],
        env_value: Value,
        docstring: Option<String>,
        doc_form: Option<Value>,
        iform_value: Value,
    ) -> Option<Value> {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return None;
        };
        if resolve_sym(hook_sym) != "cconv-make-interpreted-closure" {
            return None;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return None;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function("cconv-make-interpreted-closure")
            .cloned()
        else {
            return None;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return None;
        }

        let env_shape = interpreted_closure_env_entries(env_value);
        let iform_expr = value_to_expr(&iform_value);
        let cache_fp =
            interpreted_closure_trim_fingerprint(params_expr, body_exprs, &iform_expr, &env_shape);
        let entry = self
            .interpreted_closure_trim_cache
            .get(&cache_fp)?
            .iter()
            .find(|entry| entry.matches(params_expr, body_exprs, &iform_expr, &env_shape))?
            .clone();
        let rebuilt_env =
            rebuild_trimmed_interpreted_closure_env(env_value, &entry.trimmed_env_template);
        // Restore the interactive spec from the iform that was passed to
        // eval_lambda.  The cache key includes the iform, so the cached
        // entry corresponds to the same interactive status.
        let interactive_spec = if iform_value.is_nil() {
            None
        } else {
            Some(iform_value)
        };
        Some(Value::make_lambda(LambdaData {
            params: entry.params,
            body: entry.trimmed_body,
            env: Some(rebuilt_env),
            docstring,
            doc_form,
            interactive: interactive_spec,
        }))
    }

    fn maybe_cache_interpreted_closure_filter_result(
        &mut self,
        closure_hook: Value,
        params_expr: &Expr,
        body_exprs: &[Expr],
        env_value: Value,
        iform_value: Value,
        result: &Value,
    ) {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return;
        };
        if resolve_sym(hook_sym) != "cconv-make-interpreted-closure" {
            return;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function("cconv-make-interpreted-closure")
            .cloned()
        else {
            return;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return;
        }
        if !result.is_lambda() {
            return;
        };
        let Some(lambda_data_ref) = result.get_lambda_data() else {
            return;
        };
        let lambda_data = lambda_data_ref.clone();
        let Some(trimmed_env) = lambda_data.env else {
            return;
        };

        let env_shape = interpreted_closure_env_entries(env_value);
        let iform_expr = value_to_expr(&iform_value);
        let cache_fp =
            interpreted_closure_trim_fingerprint(params_expr, body_exprs, &iform_expr, &env_shape);
        let bucket = self
            .interpreted_closure_trim_cache
            .entry(cache_fp)
            .or_default();
        if bucket
            .iter()
            .any(|entry| entry.matches(params_expr, body_exprs, &iform_expr, &env_shape))
        {
            return;
        }
        bucket.push(InterpretedClosureTrimCacheEntry {
            params_expr: params_expr.clone(),
            body_exprs: body_exprs.to_vec(),
            iform_expr,
            env_shape,
            params: lambda_data.params,
            trimmed_body: lambda_data.body,
            trimmed_env_template: interpreted_closure_env_entries(trimmed_env),
        });
    }

    pub(crate) fn eval_lambda(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("lambda"), Value::fixnum(tail.len() as i64)],
            ));
        }

        // Extract docstring if present as the first body element.
        let (docstring, body_start) = match (tail.get(1), tail.get(2)) {
            (Some(Expr::Str(s)), Some(_)) => (Some(s.clone()), 2),
            _ => (None, 1),
        };

        // Check for (:documentation FORM) in the body — used by oclosures
        // to store the type symbol in slot 4 of the closure vector.
        let (doc_form, body_start) = if let Some(expr) = tail.get(body_start) {
            if let Expr::List(items) = expr {
                if items.len() == 2 {
                    if let Some(Expr::Keyword(kw)) = items.first() {
                        if resolve_sym(*kw) == ":documentation" {
                            let form_val = self.eval(&items[1])?;
                            (Some(form_val), body_start + 1)
                        } else {
                            (None, body_start)
                        }
                    } else {
                        (None, body_start)
                    }
                } else {
                    (None, body_start)
                }
            } else {
                (None, body_start)
            }
        } else {
            (None, body_start)
        };

        let mut body_start = body_start;
        while let Some(Expr::List(items)) = tail.get(body_start) {
            if items.first().is_some_and(
                |head| matches!(head, Expr::Symbol(id) if resolve_sym(*id) == "declare"),
            ) {
                body_start += 1;
            } else {
                break;
            }
        }

        let params_value = quote_to_value(&tail[0]);

        // GNU Emacs (eval.c:604-612): Extract (interactive ...) form from
        // the body, just like Ffunction does.  The interactive form is passed
        // separately to make-interpreted-closure / cconv-make-interpreted-closure,
        // NOT left in the body.  cconv-convert (cconv.el:844) strips
        // (interactive ...) from the body and expects it to be in iform.
        let mut iform_value = Value::NIL;
        if let Some(Expr::List(items)) = tail.get(body_start) {
            if items.first().is_some_and(
                |head| matches!(head, Expr::Symbol(id) if resolve_sym(*id) == "interactive"),
            ) {
                iform_value = quote_to_value(&tail[body_start]);
                body_start += 1;
            }
        }

        // GNU Emacs (eval.c:613-614): "Make sure the body is never empty!"
        // If no body forms, use (nil) instead of nil so cconv's
        // (cl-assert (consp body)) doesn't fail.
        let body_forms: Vec<Value> = tail[body_start..].iter().map(quote_to_value).collect();
        let body_value = if body_forms.is_empty() {
            Value::list(vec![Value::NIL])
        } else {
            Value::list(body_forms)
        };
        let env_value = if self.lexical_binding() || self.lexenv != Value::NIL {
            if self.lexenv == Value::NIL {
                Value::list(vec![Value::T])
            } else {
                self.lexenv
            }
        } else {
            Value::NIL
        };
        let docstring_value = match (&docstring, doc_form) {
            (Some(s), _) => Value::string(s.clone()),
            (None, Some(form)) => form,
            (None, None) => Value::NIL,
        };

        let scope = self.open_gc_scope();
        self.push_temp_root(params_value);
        self.push_temp_root(body_value);
        self.push_temp_root(env_value);
        self.push_temp_root(docstring_value);
        self.push_temp_root(iform_value);

        let result = if env_value != Value::NIL {
            let closure_hook =
                self.visible_variable_value_or_nil("internal-make-interpreted-closure-function");
            if closure_hook != Value::NIL {
                if let Some(cached) = self.maybe_use_cached_interpreted_closure_filter(
                    closure_hook,
                    &tail[0],
                    &tail[body_start..],
                    env_value,
                    docstring.clone(),
                    doc_form,
                    iform_value,
                ) {
                    Ok(cached)
                } else {
                    self.push_temp_root(closure_hook);
                    let result = self.apply(
                        closure_hook,
                        vec![
                            params_value,
                            body_value,
                            env_value,
                            docstring_value,
                            iform_value,
                        ],
                    );
                    self.temp_roots.pop();
                    if let Ok(value) = &result {
                        self.maybe_cache_interpreted_closure_filter_result(
                            closure_hook,
                            &tail[0],
                            &tail[body_start..],
                            env_value,
                            iform_value,
                            value,
                        );
                    }
                    result
                }
            } else {
                builtins::symbols::make_interpreted_closure_from_parts(
                    &params_value,
                    &body_value,
                    &env_value,
                    Some(&docstring_value),
                    Some(&iform_value),
                )
            }
        } else {
            builtins::symbols::make_interpreted_closure_from_parts(
                &params_value,
                &body_value,
                &env_value,
                Some(&docstring_value),
                Some(&iform_value),
            )
        };

        scope.close(self);
        result
    }

    fn parse_lambda_params(&self, expr: &Expr) -> Result<LambdaParams, Flow> {
        match expr {
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => Ok(LambdaParams::simple(vec![])),
            Expr::List(items) => {
                let mut required = Vec::new();
                let mut optional = Vec::new();
                let mut rest = None;
                let mut mode = 0; // 0=required, 1=optional, 2=rest

                for item in items {
                    // Accept both plain symbols and (SYMBOL DEFAULT) lists.
                    // GNU's defmacro stores the arglist as-is; only
                    // funcall_lambda validates at call time. cl-defmacro
                    // handles (SYMBOL DEFAULT) via macro expansion.
                    // We extract the symbol name and ignore defaults.
                    let id = match item {
                        Expr::Symbol(id) => *id,
                        Expr::List(parts) if !parts.is_empty() => {
                            if let Expr::Symbol(id) = &parts[0] {
                                *id
                            } else {
                                return Err(signal("wrong-type-argument", vec![]));
                            }
                        }
                        _ => {
                            return Err(signal("wrong-type-argument", vec![]));
                        }
                    };
                    let name = resolve_sym(id);
                    match name {
                        "&optional" => {
                            mode = 1;
                            continue;
                        }
                        "&rest" => {
                            mode = 2;
                            continue;
                        }
                        _ => {}
                    }
                    match mode {
                        0 => required.push(id),
                        1 => optional.push(id),
                        2 => {
                            rest = Some(id);
                            break;
                        }
                        _ => unreachable!(),
                    }
                }

                Ok(LambdaParams {
                    required,
                    optional,
                    rest,
                })
            }
            _ => Err(signal("wrong-type-argument", vec![])),
        }
    }

    pub(crate) fn push_runtime_backtrace_frame(&mut self, function: Value, args: &[Value]) {
        self.runtime_backtrace.push(RuntimeBacktraceFrame {
            function: self.advice_wrapper_frame_function(function),
            args: args.to_vec(),
            evaluated: true,
            debug_on_exit: false,
        });
    }

    pub(crate) fn pop_runtime_backtrace_frame(&mut self) {
        self.runtime_backtrace.pop();
    }

    pub(crate) fn with_runtime_backtrace_frame(
        &mut self,
        function: Value,
        args: &[Value],
        f: impl FnOnce(&mut Self) -> EvalResult,
    ) -> EvalResult {
        self.push_runtime_backtrace_frame(function, args);
        let result = f(self);
        self.pop_runtime_backtrace_frame();
        result
    }

    fn apply_internal(
        &mut self,
        function: Value,
        args: Vec<Value>,
        record_backtrace: bool,
    ) -> EvalResult {
        self.with_gc_scope_result(|ctx| {
            ctx.root(function);
            for &arg in &args {
                ctx.root(arg);
            }
            ctx.maybe_gc_and_quit()?;
            // Deep interpreted expansion can recurse many frames.
            // Grow the stack at the function-application boundary.
            stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || {
                if record_backtrace {
                    ctx.funcall_general(function, args)
                } else {
                    ctx.funcall_general_untraced(function, args)
                }
            })
        })
    }

    /// Apply a function value to evaluated arguments.
    pub(crate) fn apply(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        self.apply_internal(function, args, true)
    }

    pub(crate) fn apply_untraced(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        self.apply_internal(function, args, false)
    }

    /// Unified function dispatch — matches GNU Emacs's funcall_general.
    /// Called by both the tree-walking interpreter (via apply) and the
    /// bytecode VM (via Vm::call_function).
    pub(crate) fn funcall_general(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        let frame_args = args.clone();
        let frame_function = self.advice_wrapper_frame_function(function);
        self.with_runtime_backtrace_frame(frame_function, &frame_args, |eval| {
            eval.funcall_general_untraced(function, args)
        })
    }

    pub(crate) fn funcall_general_untraced(
        &mut self,
        function: Value,
        args: Vec<Value>,
    ) -> EvalResult {
        match function.kind() {
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                self.refresh_features_from_variable();
                let bc_data = function.get_bytecode_data().unwrap().clone();
                let mut vm = super::bytecode::Vm::from_context(self);
                let result = vm.execute_with_func_value(&bc_data, args, function);
                self.sync_features_variable();
                result
            }
            ValueKind::Veclike(VecLikeType::Lambda) => {
                let lambda_data = function.get_lambda_data().unwrap().clone();
                self.apply_lambda(&lambda_data, args, function)
            }
            ValueKind::Veclike(VecLikeType::Macro) => {
                let lambda_data = function.get_lambda_data().unwrap().clone();
                self.apply_lambda(&lambda_data, args, function)
            }
            ValueKind::Veclike(VecLikeType::Subr) => {
                let id = function.as_subr_id().unwrap();
                self.apply_subr_object_by_id(id, args, true)
            }
            ValueKind::Symbol(id) => self.apply_symbol_callable_untraced(id, args, true),
            ValueKind::T => self.apply_symbol_callable_untraced(intern("t"), args, true),
            ValueKind::Nil => Err(signal("void-function", vec![Value::symbol("nil")])),
            ValueKind::Cons => {
                if super::autoload::is_autoload_value(&function) {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), function],
                    ))
                } else if function.cons_car().is_symbol_named("lambda")
                    || function.cons_car().is_symbol_named("closure")
                {
                    match self.instantiate_callable_cons_form(function) {
                        Ok(callable) => self.apply(callable, args),
                        Err(_) => Err(signal("invalid-function", vec![function])),
                    }
                } else {
                    Err(signal("invalid-function", vec![function]))
                }
            }
            _ => Err(signal("invalid-function", vec![function])),
        }
    }

    /// Convert a `(lambda ...)` or `(closure ...)` cons cell into a
    /// `Value::Lambda`.  This mirrors GNU Emacs's `funcall_lambda` which
    /// handles both forms.  Used by both the interpreter and the bytecode VM.
    pub(crate) fn instantiate_callable_cons_form(&mut self, function: Value) -> EvalResult {
        let items =
            list_to_vec(&function).ok_or_else(|| signal("invalid-function", vec![function]))?;
        let Some(head_name) = items.first().and_then(|v| v.as_symbol_name()) else {
            return Err(signal("invalid-function", vec![function]));
        };

        let (env_value, params_value, mut body_start) = match head_name {
            "lambda" => {
                let Some(params_value) = items.get(1).copied() else {
                    return Err(signal("invalid-function", vec![function]));
                };
                let env_value = if self
                    .obarray
                    .symbol_value("lexical-binding")
                    .is_some_and(|value| value.is_truthy())
                    || !self.lexenv.is_nil()
                {
                    if self.lexenv.is_nil() {
                        Value::list(vec![Value::T])
                    } else {
                        self.lexenv
                    }
                } else {
                    Value::NIL
                };
                (env_value, params_value, 2)
            }
            "closure" => {
                let (Some(env_value), Some(params_value)) =
                    (items.get(1).copied(), items.get(2).copied())
                else {
                    return Err(signal("invalid-function", vec![function]));
                };
                (env_value, params_value, 3)
            }
            _ => return Err(signal("invalid-function", vec![function])),
        };

        let docstring_value = if items.get(body_start).is_some_and(|v| v.is_string())
            && items.get(body_start + 1).is_some()
        {
            let value = items[body_start];
            body_start += 1;
            value
        } else {
            Value::NIL
        };

        let mut doc_form_value = Value::NIL;
        if let Some(item) = items.get(body_start)
            && let Some(entry) = list_to_vec(item)
            && entry.len() == 2
            && entry[0].as_symbol_name() == Some(":documentation")
        {
            doc_form_value = entry[1];
            body_start += 1;
        }

        while let Some(item) = items.get(body_start) {
            let Some(declare) = list_to_vec(item) else {
                break;
            };
            if declare
                .first()
                .and_then(|v| v.as_symbol_name())
                .is_some_and(|name| name == "declare")
            {
                body_start += 1;
            } else {
                break;
            }
        }

        let body_value = if body_start >= items.len() {
            Value::NIL
        } else {
            Value::list(items[body_start..].to_vec())
        };
        let closure_doc_value = if !doc_form_value.is_nil() {
            doc_form_value
        } else {
            docstring_value
        };
        let iform_value = Value::NIL;

        let scope = self.open_gc_scope();
        self.push_temp_root(function);
        self.push_temp_root(params_value);
        self.push_temp_root(body_value);
        self.push_temp_root(env_value);
        self.push_temp_root(closure_doc_value);

        if head_name == "lambda" && !env_value.is_nil() {
            let closure_hook = self
                .obarray
                .symbol_value("internal-make-interpreted-closure-function")
                .cloned()
                .unwrap_or(Value::NIL);
            if !closure_hook.is_nil() {
                self.push_temp_root(closure_hook);
                let result = self.apply(
                    closure_hook,
                    vec![
                        params_value,
                        body_value,
                        env_value,
                        closure_doc_value,
                        iform_value,
                    ],
                );
                scope.close(self);
                return result;
            }
        }

        let result = builtins::symbols::make_interpreted_closure_from_parts(
            &params_value,
            &body_value,
            &env_value,
            Some(&closure_doc_value),
            Some(&iform_value),
        );
        scope.close(self);
        result
    }

    #[inline]
    fn apply_subr_object_by_id(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let name = resolve_sym(sym_id);
        if super::subr_info::is_special_form(name) {
            return Err(signal("invalid-function", vec![Value::subr(sym_id)]));
        }
        if super::subr_info::is_evaluator_callable_name(name) {
            return self.apply_evaluator_callable_by_id(sym_id, args);
        }
        if let Some(result) = builtins::dispatch_builtin_by_id(self, sym_id, args) {
            // Validate throws from builtins against the shared catch stack.
            let result = result.map_err(|flow| self.validate_throw(flow));
            if rewrite_builtin_wrong_arity {
                result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
            } else {
                result
            }
        } else {
            Err(signal("void-function", vec![Value::symbol(name)]))
        }
    }

    #[inline]
    fn resolve_named_call_target_by_id(&mut self, sym_id: SymId) -> NamedCallTarget {
        let function_epoch = self.obarray.function_epoch();
        if self
            .named_call_cache
            .first()
            .is_some_and(|cache| cache.function_epoch != function_epoch)
        {
            self.named_call_cache.clear();
        }
        if let Some(cache) = self
            .named_call_cache
            .iter()
            .find(|cache| cache.symbol == sym_id && cache.function_epoch == function_epoch)
        {
            return cache.target.clone();
        }

        let name = resolve_sym(sym_id);
        let target = if let Some(func) = self.obarray.symbol_function_id(sym_id).cloned() {
            match func.kind() {
                ValueKind::Nil => NamedCallTarget::Void,
                // `(fset 'foo (symbol-function 'foo))` writes `#<subr foo>` into
                // the function cell. Treat this as a direct builtin/special-form
                // callable, not an obarray indirection cycle.
                ValueKind::Veclike(VecLikeType::Subr)
                    if resolve_sym(func.as_subr_id().unwrap()) == name =>
                {
                    if super::subr_info::is_evaluator_callable_name(name) {
                        NamedCallTarget::ContextCallable
                    } else if super::subr_info::is_special_form(name) {
                        NamedCallTarget::SpecialForm
                    } else {
                        NamedCallTarget::Builtin
                    }
                }
                _ => NamedCallTarget::Obarray(func),
            }
        } else if self.obarray.is_function_unbound_id(sym_id) {
            NamedCallTarget::Void
        } else if self.has_registered_subr(intern(name)) {
            NamedCallTarget::Builtin
        } else {
            NamedCallTarget::Void
        };

        if self.named_call_cache.len() == NAMED_CALL_CACHE_CAPACITY {
            self.named_call_cache.remove(0);
        }
        self.named_call_cache.push(NamedCallCache {
            symbol: sym_id,
            function_epoch,
            target: target.clone(),
        });

        target
    }

    #[inline]
    fn resolve_named_call_target(&mut self, name: &str) -> NamedCallTarget {
        self.resolve_named_call_target_by_id(intern(name))
    }

    #[inline]
    fn store_named_call_cache(&mut self, symbol: SymId, target: NamedCallTarget) {
        let function_epoch = self.obarray.function_epoch();
        if self
            .named_call_cache
            .first()
            .is_some_and(|cache| cache.function_epoch != function_epoch)
        {
            self.named_call_cache.clear();
        }
        if let Some(slot) = self
            .named_call_cache
            .iter_mut()
            .find(|cache| cache.symbol == symbol)
        {
            slot.function_epoch = function_epoch;
            slot.target = target;
            return;
        }
        if self.named_call_cache.len() == NAMED_CALL_CACHE_CAPACITY {
            self.named_call_cache.remove(0);
        }
        self.named_call_cache.push(NamedCallCache {
            symbol,
            function_epoch,
            target,
        });
    }

    #[inline]
    fn apply_named_callable_by_id(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let frame_args = args.clone();
        self.with_runtime_backtrace_frame(Value::from_sym_id(sym_id), &frame_args, |eval| {
            eval.apply_named_callable_by_id_core(
                sym_id,
                args,
                invalid_fn,
                rewrite_builtin_wrong_arity,
            )
        })
    }

    #[inline]
    fn apply_named_callable(
        &mut self,
        name: &str,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let frame_function = Value::symbol(name);
        let frame_args = args.clone();
        self.with_runtime_backtrace_frame(frame_function, &frame_args, |eval| {
            eval.apply_named_callable_core(name, args, invalid_fn, rewrite_builtin_wrong_arity)
        })
    }

    fn apply_named_callable_by_id_core(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let name = resolve_sym(sym_id);
        match self.resolve_named_call_target_by_id(sym_id) {
            NamedCallTarget::Obarray(func) => {
                if super::autoload::is_autoload_value(&func) {
                    return self.apply_named_autoload_callable(
                        name,
                        func,
                        args,
                        rewrite_builtin_wrong_arity,
                    );
                }
                let function_is_callable = self.function_value_is_callable(&func);
                let alias_target = match func.kind() {
                    ValueKind::Symbol(target) => Some(resolve_sym(target).to_owned()),
                    ValueKind::Veclike(VecLikeType::Subr) => {
                        let bound_name = func.as_subr_id().unwrap();
                        if resolve_sym(bound_name) != name {
                            Some(resolve_sym(bound_name).to_owned())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let result = match self.apply_untraced(func, args) {
                    Err(Flow::Signal(sig))
                        if sig.symbol_name() == "invalid-function" && !function_is_callable =>
                    {
                        Err(signal("invalid-function", vec![Value::symbol(name)]))
                    }
                    other => other,
                };
                if let Some(target) = alias_target {
                    if rewrite_builtin_wrong_arity {
                        result
                    } else {
                        result.map_err(|flow| {
                            rewrite_wrong_arity_alias_function_object(flow, name, &target)
                        })
                    }
                } else {
                    result
                }
            }
            NamedCallTarget::ContextCallable => self.apply_evaluator_callable_by_id(sym_id, args),
            NamedCallTarget::Builtin => {
                if let Some(result) = builtins::dispatch_builtin_by_id(self, sym_id, args) {
                    let result = result.map_err(|flow| self.validate_throw(flow));
                    if rewrite_builtin_wrong_arity {
                        result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                    } else {
                        result
                    }
                } else {
                    self.store_named_call_cache(sym_id, NamedCallTarget::Void);
                    Err(signal("void-function", vec![Value::symbol(name)]))
                }
            }
            NamedCallTarget::SpecialForm => Err(signal("invalid-function", vec![invalid_fn])),
            NamedCallTarget::Void => Err(signal("void-function", vec![Value::symbol(name)])),
        }
    }

    fn apply_named_callable_core(
        &mut self,
        name: &str,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        match self.resolve_named_call_target(name) {
            NamedCallTarget::Obarray(func) => {
                if super::autoload::is_autoload_value(&func) {
                    return self.apply_named_autoload_callable(
                        name,
                        func,
                        args,
                        rewrite_builtin_wrong_arity,
                    );
                }
                let function_is_callable = self.function_value_is_callable(&func);
                let alias_target = match func.kind() {
                    ValueKind::Symbol(target) => Some(resolve_sym(target).to_owned()),
                    ValueKind::Veclike(VecLikeType::Subr) => {
                        let bound_name = func.as_subr_id().unwrap();
                        if resolve_sym(bound_name) != name {
                            Some(resolve_sym(bound_name).to_owned())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                let result = match self.apply(func, args) {
                    Err(Flow::Signal(sig))
                        if sig.symbol_name() == "invalid-function" && !function_is_callable =>
                    {
                        Err(signal("invalid-function", vec![Value::symbol(name)]))
                    }
                    other => other,
                };
                if let Some(target) = alias_target {
                    if rewrite_builtin_wrong_arity {
                        result
                    } else {
                        result.map_err(|flow| {
                            rewrite_wrong_arity_alias_function_object(flow, name, &target)
                        })
                    }
                } else {
                    result
                }
            }
            NamedCallTarget::ContextCallable => self.apply_evaluator_callable(name, args),
            NamedCallTarget::Builtin => {
                if let Some(result) = builtins::dispatch_builtin(self, name, args) {
                    let result = result.map_err(|flow| self.validate_throw(flow));
                    if rewrite_builtin_wrong_arity {
                        result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                    } else {
                        result
                    }
                } else {
                    self.store_named_call_cache(intern(name), NamedCallTarget::Void);
                    Err(signal("void-function", vec![Value::symbol(name)]))
                }
            }
            NamedCallTarget::SpecialForm => Err(signal("invalid-function", vec![invalid_fn])),
            NamedCallTarget::Void => Err(signal("void-function", vec![Value::symbol(name)])),
        }
    }

    fn apply_named_autoload_callable(
        &mut self,
        name: &str,
        autoload_form: Value,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        // Startup wrappers often expose autoload-shaped function cells for names
        // backed by builtins. Keep the autoload shape while preserving callability.
        let sym_id = intern(name);
        if self.has_registered_subr(sym_id) {
            if let Some(result) = builtins::dispatch_builtin_by_id(self, sym_id, args.clone()) {
                return if rewrite_builtin_wrong_arity {
                    result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                } else {
                    result
                };
            }
        }

        let loaded = super::autoload::builtin_autoload_do_load(
            self,
            vec![autoload_form, Value::symbol(name)],
        )?;
        let function_is_callable = self.function_value_is_callable(&loaded);
        match self.apply_untraced(loaded, args) {
            Err(Flow::Signal(sig))
                if sig.symbol_name() == "invalid-function" && !function_is_callable =>
            {
                Err(signal("invalid-function", vec![Value::symbol(name)]))
            }
            other => other,
        }
    }

    fn apply_evaluator_callable(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match name {
            "throw" => {
                if args.len() != 2 {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![
                            Value::subr(intern("throw")),
                            Value::fixnum(args.len() as i64),
                        ],
                    ));
                }
                let tag = args[0];
                let value = args[1];
                if self.has_active_catch(&tag) {
                    Err(Flow::Throw { tag, value })
                } else {
                    Err(signal("no-catch", vec![tag, value]))
                }
            }
            _ => Err(signal("void-function", vec![Value::symbol(name)])),
        }
    }

    fn apply_evaluator_callable_by_id(&mut self, sym_id: SymId, args: Vec<Value>) -> EvalResult {
        self.apply_evaluator_callable(resolve_sym(sym_id), args)
    }

    fn apply_lambda(
        &mut self,
        lambda: &LambdaData,
        args: Vec<Value>,
        func_value: Value,
    ) -> EvalResult {
        let call_state = self.begin_lambda_call(lambda, &args, func_value)?;
        let result = self.eval_lambda_body(&lambda.body);
        self.finish_lambda_call(call_state);
        result
    }

    #[inline]
    fn bind_lexical_value_rooted(&mut self, sym: SymId, value: Value) {
        bind_lexical_value_rooted_in_state(&mut self.lexenv, &mut self.temp_roots, sym, value);
    }

    // -----------------------------------------------------------------------
    // Macro expansion
    // -----------------------------------------------------------------------

    pub(crate) fn expand_macro(
        &mut self,
        macro_val: Value,
        args: &[Expr],
    ) -> Result<Rc<Expr>, Flow> {
        if !macro_val.is_macro() {
            return Err(signal("invalid-macro", vec![]));
        };

        // Check cache: same macro object + same source location (args slice
        // pointer from Rc<Vec<Expr>> body) → same expansion.
        // Fingerprint validation detects ABA from reused addresses.
        let cache_key = (
            macro_val.bits(),
            args.as_ptr() as usize,
            self.macro_expansion_context_key(),
        );
        let current_fp = tail_fingerprint(args);
        if !self.macro_cache_disabled {
            if let Some(cached) = self.macro_expansion_cache.get(&cache_key).cloned() {
                if cached.fingerprint == current_fp {
                    self.macro_cache_hits += 1;
                    return Ok(cached.expanded.clone());
                }
                // Fingerprint mismatch → ABA, fall through to re-expand
            }
        }

        let expand_start = std::time::Instant::now();
        // Clone the macro data before calling self.apply_lambda
        let lambda_data = macro_val.get_lambda_data().unwrap().clone();

        // Root arg values during macro expansion to survive GC.
        // Use cached source-literal materialization so that the same Expr
        // pointer (from a shared Rc<Vec<Expr>> lambda body) produces the same
        // runtime Value object when re-evaluated.
        let scope = self.open_gc_scope();
        let arg_values: Vec<Value> = args
            .iter()
            .map(|e| self.cached_source_literal_to_value(e))
            .collect();
        for v in &arg_values {
            self.push_temp_root(*v);
        }

        let expanded_value = self.with_macro_expansion_scope(|eval| {
            eval.apply_lambda(&lambda_data, arg_values, macro_val)
        })?;
        // Root expansion result during value_to_expr traversal
        self.push_temp_root(expanded_value);

        // Convert value back to expr for re-evaluation
        let result = Rc::new(value_to_expr(&expanded_value));
        scope.close(self);

        // Cache the expansion as Rc<Expr>.  The Rc keeps the expansion
        // data alive, so inner Vec addresses remain stable for future
        // cache key lookups by inner macro calls.
        let expand_elapsed = expand_start.elapsed();
        self.macro_cache_misses += 1;
        self.macro_expand_total_us += expand_elapsed.as_micros() as u64;
        let cache_entry = Rc::new(MacroExpansionCacheEntry::new(result.clone(), current_fp));
        // With OpaqueValuePool, expansions containing OpaqueValueRef are
        // safe to cache — the pool keeps referenced Values GC-rooted.
        if !self.macro_cache_disabled {
            if expand_elapsed.as_millis() > 50 {
                tracing::warn!(
                    "macro_cache MISS macro={:#x} ptr={:#x} took {expand_elapsed:.2?}",
                    macro_val.bits(),
                    args.as_ptr() as usize
                );
            }
            self.macro_expansion_cache.insert(cache_key, cache_entry);
        }

        Ok(result)
    }

    pub(crate) fn with_macro_expansion_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        let state = begin_macro_expansion_scope_in_state(
            &mut self.obarray,
            &self.specpdl,
            &mut self.buffers,
            &self.custom,
            self.lexenv,
            &mut self.temp_roots,
        );
        let result = f(self);
        finish_macro_expansion_scope_in_state(
            &mut self.obarray,
            &self.specpdl,
            &mut self.buffers,
            &self.custom,
            &mut self.temp_roots,
            state,
        );
        result
    }

    fn macro_expansion_context_key(&self) -> u64 {
        fn value_identity_key(value: Value) -> u64 {
            match value.kind() {
                ValueKind::Nil => 0,
                ValueKind::T => 1,
                ValueKind::Fixnum(n) => ((n as u64).wrapping_mul(0x9E37_79B1)) ^ 0x10,
                ValueKind::Symbol(sym) => ((sym.0 as u64) << 8) ^ 0x20,
                ValueKind::Veclike(VecLikeType::Subr) => {
                    let sym = value.as_subr_id().unwrap();
                    ((sym.0 as u64) << 8) ^ 0x22
                }
                _ => (value.bits() as u64) ^ 0x30,
            }
        }

        value_identity_key(
            self.obarray()
                .symbol_value("macroexpand-all-environment")
                .copied()
                .unwrap_or(Value::NIL),
        )
    }

    // -----------------------------------------------------------------------
    // Variable assignment
    // -----------------------------------------------------------------------

    // Shared runtime write path for symbol-cell mutation. This mirrors GNU
    // `set_internal` after lexical handling has already been decided.

    // -----------------------------------------------------------------------
    // specbind / unbind_to — GNU Emacs specpdl-style dynamic variable binding
    // -----------------------------------------------------------------------

    /// Save the current value of a special variable and set a new value.
    /// Matches GNU Emacs's specbind() in eval.c:
    /// - Follows SYMBOL_VARALIAS to the final target
    /// - For buffer-local variables with a local binding: SPECPDL_LET_LOCAL
    /// - For buffer-local variables without local binding: SPECPDL_LET_DEFAULT
    /// - For plain variables: SPECPDL_LET
    pub(crate) fn specbind(&mut self, sym_id: SymId, value: Value) {
        let resolved =
            builtins::resolve_variable_alias_id_in_obarray(&self.obarray, sym_id).unwrap_or(sym_id);
        let name = resolve_sym(resolved);
        // Debug: trace when macroexpand-all-environment gets a non-list value
        if name == "macroexpand-all-environment" && !value.is_nil() && !value.is_cons() {
            tracing::error!(
                "specbind macroexpand-all-environment to non-list: {:?} bits={:#x}",
                value.kind(),
                value.bits()
            );
        }

        // Check if this is a buffer-local variable (GNU: SYMBOL_LOCALIZED path)
        if self.obarray.is_buffer_local(name) || self.custom.is_auto_buffer_local(name) {
            if let Some(buf_id) = self.buffers.current_buffer_id() {
                if let Some(buf) = self.buffers.get(buf_id) {
                    if let Some(binding) = buf.get_buffer_local_binding(name) {
                        // Buffer HAS local binding → SPECPDL_LET_LOCAL
                        let old_val = binding.as_value().unwrap_or(Value::NIL);
                        self.specpdl.push(SpecBinding::LetLocal {
                            sym_id: resolved,
                            old_value: old_val,
                            buffer_id: buf_id,
                        });
                        if self.watchers.has_watchers(name) {
                            let _ = self.run_variable_watchers(name, &value, &Value::NIL, "let");
                        }
                        let _ = self.buffers.set_buffer_local_property(buf_id, name, value);
                        return;
                    }
                }
            }
            // Buffer has NO local binding → SPECPDL_LET_DEFAULT
            // Save/restore the default value, don't create a buffer-local binding.
            // This matches GNU: let-binding doesn't auto-create locals.
            let old_default = self.obarray.default_value_id(resolved).copied();
            self.specpdl.push(SpecBinding::LetDefault {
                sym_id: resolved,
                old_value: old_default,
            });
            if self.watchers.has_watchers(name) {
                let _ = self.run_variable_watchers(name, &value, &Value::NIL, "let");
            }
            self.obarray.set_symbol_value_id(resolved, value);
            return;
        }

        // Plain value path (GNU: SYMBOL_PLAINVAL)
        let old_value = self.obarray.symbol_value_id(resolved).copied();
        self.specpdl.push(SpecBinding::Let {
            sym_id: resolved,
            old_value,
        });
        if self.watchers.has_watchers(name) {
            let _ = self.run_variable_watchers(name, &value, &Value::NIL, "let");
        }
        self.obarray.set_symbol_value_id(resolved, value);
    }

    /// Check if a `let` is currently shadowing a buffer-local variable's
    /// default value. Matches GNU's `let_shadows_buffer_binding_p()`.
    /// When true, `setq` inside the `let` should modify the default,
    /// NOT auto-create a buffer-local binding.
    pub(crate) fn let_shadows_buffer_binding_p(&self, sym_id: SymId) -> bool {
        self.specpdl
            .iter()
            .rev()
            .any(|entry| matches!(entry, SpecBinding::LetDefault { sym_id: s, .. } if *s == sym_id))
    }

    /// Restore all specpdl bindings back to `count`.
    /// Matches GNU Emacs's unbind_to() in eval.c.
    pub(crate) fn unbind_to(&mut self, count: usize) {
        while self.specpdl.len() > count {
            let binding = self.specpdl.pop().unwrap();
            match binding {
                SpecBinding::Let { sym_id, old_value } => {
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(name) {
                        let restore_val = old_value.unwrap_or(Value::NIL);
                        let _ =
                            self.run_variable_watchers(name, &restore_val, &Value::NIL, "unlet");
                    }
                    match old_value {
                        Some(val) => self.obarray.set_symbol_value_id(sym_id, val),
                        None => self.obarray.makunbound_id(sym_id),
                    }
                }
                SpecBinding::LetLocal {
                    sym_id,
                    old_value,
                    buffer_id,
                } => {
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(name) {
                        let _ = self.run_variable_watchers(name, &old_value, &Value::NIL, "unlet");
                    }
                    // Restore only if the buffer is still live
                    // (GNU: check Flocal_variable_p before restoring)
                    if self.buffers.get(buffer_id).is_some() {
                        let _ = self
                            .buffers
                            .set_buffer_local_property(buffer_id, name, old_value);
                    }
                }
                SpecBinding::LetDefault { sym_id, old_value } => {
                    // Restore the default value (GNU: set_default_internal)
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(name) {
                        let restore_val = old_value.unwrap_or(Value::NIL);
                        let _ =
                            self.run_variable_watchers(name, &restore_val, &Value::NIL, "unlet");
                    }
                    match old_value {
                        Some(val) => self.obarray.set_symbol_value_id(sym_id, val),
                        None => self.obarray.makunbound_id(sym_id),
                    }
                }
            }
        }
    }
}

/// Save the current value of a special variable and set a new value (standalone version).
/// Used by bytecode VM and other split-state paths.
/// Follows variable aliases like GNU's specbind().
pub(crate) fn specbind_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    sym_id: SymId,
    value: Value,
) {
    let resolved =
        super::builtins::resolve_variable_alias_id_in_obarray(obarray, sym_id).unwrap_or(sym_id);
    let old_value = obarray.symbol_value_id(resolved).copied();
    specpdl.push(SpecBinding::Let {
        sym_id: resolved,
        old_value,
    });
    obarray.set_symbol_value_id(resolved, value);
}

/// Restore all specpdl bindings back to `count` (standalone version).
/// Used by bytecode VM and other split-state paths.
/// Note: LetLocal bindings require a buffer manager; the standalone version
/// only handles Let bindings. LetLocal in the VM is not expected since
/// the VM's VarBind opcode doesn't produce buffer-local bindings.
pub(crate) fn unbind_to_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    count: usize,
) {
    while specpdl.len() > count {
        let binding = specpdl.pop().unwrap();
        match binding {
            SpecBinding::Let { sym_id, old_value } => match old_value {
                Some(val) => obarray.set_symbol_value_id(sym_id, val),
                None => obarray.makunbound_id(sym_id),
            },
            SpecBinding::LetLocal {
                sym_id, old_value, ..
            } => {
                // Standalone path doesn't have buffer manager access.
                // Fall back to setting the obarray default value.
                tracing::warn!(
                    "unbind_to_in_state: LetLocal for {} without buffer manager",
                    resolve_sym(sym_id)
                );
                obarray.set_symbol_value_id(sym_id, old_value);
            }
            SpecBinding::LetDefault { sym_id, old_value } => match old_value {
                Some(val) => obarray.set_symbol_value_id(sym_id, val),
                None => obarray.makunbound_id(sym_id),
            },
        }
    }
}

fn default_toplevel_binding(specpdl: &[SpecBinding], sym_id: SymId) -> Option<&SpecBinding> {
    specpdl.iter().find(|binding| match binding {
        SpecBinding::Let {
            sym_id: binding_sym,
            ..
        }
        | SpecBinding::LetDefault {
            sym_id: binding_sym,
            ..
        } => *binding_sym == sym_id,
        SpecBinding::LetLocal { .. } => false,
    })
}

pub(crate) fn default_toplevel_value_in_state(
    obarray: &Obarray,
    specpdl: &[SpecBinding],
    sym_id: SymId,
) -> Option<Value> {
    match default_toplevel_binding(specpdl, sym_id) {
        Some(SpecBinding::Let { old_value, .. })
        | Some(SpecBinding::LetDefault { old_value, .. }) => *old_value,
        Some(SpecBinding::LetLocal { .. }) => unreachable!("local bindings are excluded above"),
        None => obarray.default_value_id(sym_id).copied(),
    }
}

pub(crate) fn set_default_toplevel_value_in_state(
    specpdl: &mut [SpecBinding],
    sym_id: SymId,
    value: Value,
) -> bool {
    for binding in specpdl.iter_mut() {
        match binding {
            SpecBinding::Let {
                sym_id: binding_sym,
                old_value,
            }
            | SpecBinding::LetDefault {
                sym_id: binding_sym,
                old_value,
            } if *binding_sym == sym_id => {
                *old_value = Some(value);
                return true;
            }
            SpecBinding::Let { .. }
            | SpecBinding::LetDefault { .. }
            | SpecBinding::LetLocal { .. } => {}
        }
    }
    false
}

pub(crate) fn set_runtime_binding_in_state(
    ctx: &mut Context,
    sym_id: SymId,
    value: Value,
) -> Option<crate::buffer::BufferId> {
    set_runtime_binding(
        &mut ctx.obarray,
        &mut ctx.buffers,
        &ctx.custom,
        ctx.specpdl.as_slice(),
        sym_id,
        value,
    )
}

pub(crate) fn set_runtime_binding(
    obarray: &mut Obarray,
    buffers: &mut BufferManager,
    custom: &CustomManager,
    specpdl: &[SpecBinding],
    sym_id: SymId,
    value: Value,
) -> Option<crate::buffer::BufferId> {
    let name = resolve_sym(sym_id);
    let symbol_is_canonical = super::builtins::is_canonical_symbol_id(sym_id);

    // If the buffer already has a local binding, always write to it.
    if symbol_is_canonical
        && let Some(current_id) = buffers.current_buffer_id()
        && let Some(buf) = buffers.get(current_id)
    {
        if buf.has_buffer_local(name) {
            let _ = buffers.set_buffer_local_property(current_id, name, value);
            return Some(current_id);
        }
    }

    // If the variable is buffer-local (local_if_set=true) and no local
    // binding exists yet, auto-create one — UNLESS a `let` is currently
    // shadowing the default value (GNU: let_shadows_buffer_binding_p).
    if symbol_is_canonical && (obarray.is_buffer_local(name) || custom.is_auto_buffer_local(name)) {
        // Check if a let is shadowing the default value
        let let_shadows = specpdl.iter().rev().any(
            |entry| matches!(entry, SpecBinding::LetDefault { sym_id: s, .. } if *s == sym_id),
        );
        if !let_shadows {
            if let Some(current_id) = buffers.current_buffer_id() {
                let _ = buffers.set_buffer_local_property(current_id, name, value);
                return Some(current_id);
            }
        }
    }

    obarray.set_symbol_value_id(sym_id, value);
    None
}

pub(crate) fn makunbound_runtime_binding_in_state(
    obarray: &mut Obarray,
    buffers: &mut BufferManager,
    custom: &CustomManager,
    _specpdl: &[SpecBinding],
    sym_id: SymId,
) {
    let name = resolve_sym(sym_id);
    let symbol_is_canonical = super::builtins::is_canonical_symbol_id(sym_id);

    // specbind writes directly to obarray, so no dynamic frame lookup needed.

    if symbol_is_canonical
        && let Some(current_id) = buffers.current_buffer_id()
        && let Some(buf) = buffers.get(current_id)
        && buf.has_buffer_local(name)
    {
        let _ = buffers.set_buffer_local_void_property(current_id, name);
        return;
    }

    if symbol_is_canonical && (obarray.is_buffer_local(name) || custom.is_auto_buffer_local(name)) {
        if let Some(current_id) = buffers.current_buffer_id() {
            let _ = buffers.set_buffer_local_void_property(current_id, name);
            return;
        }
    }

    obarray.makunbound_id(sym_id);
}

impl Context {
    pub(crate) fn materialize_public_evaluator_function_cells(&mut self) {
        // GNU `defsubr` installs public special forms and evaluator callables
        // directly into the symbol's function cell. Keep those cells
        // authoritative instead of synthesizing them later from name tables.
        for name in super::subr_info::public_evaluator_subr_names() {
            let sym_id = intern(name);
            self.obarray.intern(name);
            self.obarray
                .set_symbol_function_id(sym_id, Value::subr(sym_id));
        }
    }

    // -----------------------------------------------------------------------
    // defsubr — builtin function registration (matches GNU Emacs's defsubr)
    // -----------------------------------------------------------------------

    /// Register a builtin function by name, storing a function pointer in the
    /// registry. At call time, the function pointer is invoked directly — no
    /// string-matching dispatch needed.
    pub fn defsubr(
        &mut self,
        name: &str,
        func: fn(&mut Context, Vec<Value>) -> EvalResult,
        min_args: u16,
        max_args: Option<u16>,
    ) {
        let sym_id = intern(name);
        self.register_subr_slot(
            sym_id,
            SubrObject {
                function: func,
                min_args,
                max_args,
                name: sym_id,
            },
        );
        // Like GNU Emacs's defsubr: set the symbol's function cell in the
        // obarray so that fboundp, symbol-function, etc. find the builtin
        // without needing a separate name registry.
        self.obarray.intern(name);
        self.obarray.set_symbol_function(name, Value::subr(sym_id));
    }

    /// Look up a builtin in the subr registry and call it directly via
    /// function pointer. Returns None if the name is not registered.
    pub fn dispatch_subr_id(&mut self, sym_id: SymId, args: Vec<Value>) -> Option<EvalResult> {
        let subr = self.subr_slot(sym_id)?;
        let name = resolve_sym(sym_id);
        // Debug: trace (cdr t) to find the calling context
        if name == "cdr" && args.len() == 1 && args[0].is_t() {
            tracing::error!("(cdr t) called! Lisp backtrace:");
            for (i, frame) in self.runtime_backtrace.iter().rev().take(10).enumerate() {
                let func_name = super::print::print_value(&frame.function);
                tracing::error!("  bt[{}]: {}", i, func_name);
            }
        }
        let nargs = args.len();
        if (nargs as u16) < subr.min_args {
            return Some(Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(name), Value::fixnum(nargs as i64)],
            )));
        }
        if let Some(max) = subr.max_args {
            if nargs as u16 > max {
                return Some(Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::symbol(name), Value::fixnum(nargs as i64)],
                )));
            }
        }
        // SAFETY: we need to call a fn(&mut Context, Vec<Value>) where the
        // function pointer is stored inside self.subr_registry (which is part
        // of self). We copy the function pointer out first to avoid borrowing
        // self immutably (for the lookup) and mutably (for the call) at the
        // same time.
        let func = subr.function;
        Some(func(self, args))
    }

    pub fn dispatch_subr(&mut self, name: &str, args: Vec<Value>) -> Option<EvalResult> {
        self.dispatch_subr_id(intern(name), args)
    }

    // -----------------------------------------------------------------------
    // Methods previously on VmSharedState, now on Context directly
    // -----------------------------------------------------------------------

    /// Run a closure with extra GC roots pushed onto the temp root stack.
    /// Used when crossing from the bytecode VM into evaluator code that may
    /// trigger garbage collection.
    pub(crate) fn with_extra_gc_roots<T>(
        &mut self,
        vm_gc_roots: &[Value],
        extra_roots: &[Value],
        f: impl FnOnce(&mut Context) -> T,
    ) -> T {
        self.with_gc_scope(|ctx| {
            for root in vm_gc_roots {
                ctx.push_temp_root(*root);
            }
            for root in extra_roots {
                ctx.push_temp_root(*root);
            }
            f(ctx)
        })
    }

    pub(crate) fn begin_eval_with_lexical_arg(
        &mut self,
        lexical_arg: Option<Value>,
    ) -> Result<ActiveEvalLexicalArgState, Flow> {
        begin_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            lexical_arg,
        )
    }

    pub(crate) fn finish_eval_with_lexical_arg(&mut self, state: ActiveEvalLexicalArgState) {
        finish_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.saved_lexenvs,
            state,
        );
    }

    pub(crate) fn begin_macro_expansion_scope(&mut self) -> ActiveMacroExpansionScopeState {
        begin_macro_expansion_scope_in_state(
            &mut self.obarray,
            &self.specpdl,
            &mut self.buffers,
            &self.custom,
            self.lexenv,
            &mut self.temp_roots,
        )
    }

    pub(crate) fn finish_macro_expansion_scope(&mut self, state: ActiveMacroExpansionScopeState) {
        finish_macro_expansion_scope_in_state(
            &mut self.obarray,
            &self.specpdl,
            &mut self.buffers,
            &self.custom,
            &mut self.temp_roots,
            state,
        );
    }

    pub(crate) fn kmacro_mut(&mut self) -> &mut KmacroManager {
        &mut self.kmacro
    }

    pub(crate) fn gui_frame_creation_state(
        &mut self,
    ) -> (
        &mut FrameManager,
        &mut BufferManager,
        &mut Option<Box<dyn DisplayHost>>,
    ) {
        (&mut self.frames, &mut self.buffers, &mut self.display_host)
    }

    pub(crate) fn recursive_command_loop_depth(&self) -> usize {
        self.command_loop.recursive_depth
    }

    // -----------------------------------------------------------------------

    /// Assign a value to a variable identified by SymId.
    /// Uses the SymId directly for lexenv/dynamic lookup, preserving
    /// uninterned symbol identity (like Emacs's EQ-based setq).
    pub(crate) fn assign_by_id(&mut self, sym_id: SymId, value: Value) {
        let _ = self.assign_by_id_with_locus(sym_id, value);
    }

    pub(crate) fn assign_by_id_with_locus(
        &mut self,
        sym_id: SymId,
        value: Value,
    ) -> Option<crate::buffer::BufferId> {
        let name = resolve_sym(sym_id);
        // If lexical binding and not special, check lexenv first
        // GNU `setq` follows the same rule as `eval_sub`: if a lexical binding
        // cell exists, mutate it directly; don't rescan bare-symbol dynvar
        // declarations at assignment time.
        if self.lexical_binding() && !is_runtime_dynamically_special(&self.obarray, sym_id) {
            if let Some(cell_id) = lexenv_assq(self.lexenv, sym_id) {
                lexenv_set(cell_id, value);
                return None;
            }
        }

        let locus = set_runtime_binding(
            &mut self.obarray,
            &mut self.buffers,
            &self.custom,
            &self.specpdl,
            sym_id,
            value,
        );
        self.sync_keyboard_runtime_binding_by_id(sym_id, value);
        locus
    }

    pub(crate) fn assign(&mut self, name: &str, value: Value) {
        self.assign_by_id(intern(name), value);
    }

    pub(crate) fn set_runtime_binding_by_id(
        &mut self,
        sym_id: SymId,
        value: Value,
    ) -> Option<crate::buffer::BufferId> {
        let locus = set_runtime_binding(
            &mut self.obarray,
            &mut self.buffers,
            &self.custom,
            &self.specpdl,
            sym_id,
            value,
        );
        self.sync_keyboard_runtime_binding_by_id(sym_id, value);
        locus
    }

    pub(crate) fn makunbound_runtime_binding_by_id(&mut self, sym_id: SymId) {
        makunbound_runtime_binding_in_state(
            &mut self.obarray,
            &mut self.buffers,
            &self.custom,
            &[],
            sym_id,
        );
        self.sync_keyboard_runtime_binding_by_id(sym_id, Value::NIL);
    }

    fn has_local_binding_by_id(&self, sym_id: SymId) -> bool {
        lexenv_assq(self.lexenv, sym_id).is_some()
            || self
                .specpdl
                .iter()
                .rev()
                .any(|entry| matches!(entry, SpecBinding::Let { sym_id: s, .. } if *s == sym_id))
    }

    pub(crate) fn visible_variable_value_or_nil(&self, name: &str) -> Value {
        let name_id = intern(name);
        if !is_runtime_dynamically_special(&self.obarray, name_id)
            && let Some(value) = lexenv_lookup(self.lexenv, name_id)
        {
            return value;
        }
        // specbind writes directly to obarray, so no dynamic stack lookup needed.
        if let Some(buffer) = self.buffers.current_buffer() {
            if let Some(binding) = buffer.get_buffer_local_binding(name) {
                return binding.as_value().unwrap_or(Value::NIL);
            }
        }
        if let Some(value) = self.obarray.symbol_value(name).cloned() {
            return value;
        }
        if name == "nil" {
            return Value::NIL;
        }
        if name == "t" {
            return Value::T;
        }
        Value::NIL
    }

    pub(crate) fn visible_runtime_variable_value_by_id(
        &self,
        sym_id: SymId,
    ) -> Result<Option<Value>, Flow> {
        let resolved = builtins::resolve_variable_alias_id_in_obarray(&self.obarray, sym_id)?;
        Ok(self.visible_runtime_variable_value_by_id_resolved(resolved))
    }

    pub(crate) fn visible_runtime_variable_value_by_id_resolved(
        &self,
        resolved: SymId,
    ) -> Option<Value> {
        let resolved_name = resolve_sym(resolved);
        let resolved_is_canonical = builtins::is_canonical_symbol_id(resolved);

        if resolved_is_canonical
            && let Some(buf) = self.buffers.current_buffer()
            && let Some(binding) = buf.get_buffer_local_binding(resolved_name)
        {
            return binding.as_value();
        }

        if let Some(value) = self.obarray.symbol_value_id(resolved).copied() {
            return Some(value);
        }

        if resolved_is_canonical && resolved_name == "nil" {
            return Some(Value::NIL);
        }
        if resolved_is_canonical && resolved_name == "t" {
            return Some(Value::T);
        }
        if resolved_is_canonical && resolved_name.starts_with(':') {
            return Some(Value::from_kw_id(resolved));
        }

        None
    }

    fn run_unlet_watchers(&mut self, bindings: &[(String, Value, Value)]) -> Result<(), Flow> {
        for (name, _, restored_value) in bindings.iter().rev() {
            self.run_variable_watchers(name, restored_value, &Value::NIL, "unlet")?;
        }
        Ok(())
    }

    pub(crate) fn run_variable_watchers(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_with_where(name, new_value, old_value, operation, &Value::NIL)
    }

    pub(crate) fn run_variable_watchers_with_where(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        if !self.watchers.has_watchers(name) {
            return Ok(());
        }
        let calls =
            self.watchers
                .notify_watchers(name, new_value, old_value, operation, where_value);
        for (callback, args) in calls {
            let _ = self.apply(callback, args)?;
        }
        Ok(())
    }

    pub(crate) fn assign_with_watchers(
        &mut self,
        name: &str,
        value: Value,
        operation: &str,
    ) -> EvalResult {
        let where_value = self
            .assign_by_id_with_locus(intern(name), value)
            .map(Value::make_buffer)
            .unwrap_or(Value::NIL);
        self.run_variable_watchers_with_where(name, &value, &Value::NIL, operation, &where_value)?;
        Ok(value)
    }

    pub(crate) fn assign_with_watchers_by_id(
        &mut self,
        sym_id: SymId,
        value: Value,
        operation: &str,
    ) -> EvalResult {
        let where_value = self
            .assign_by_id_with_locus(sym_id, value)
            .map(Value::make_buffer)
            .unwrap_or(Value::NIL);
        let name = resolve_sym(sym_id);
        self.run_variable_watchers_with_where(name, &value, &Value::NIL, operation, &where_value)?;
        Ok(value)
    }

    /// Cached version of quote construction keyed on `Expr` pointer identity.
    ///
    /// When the same `&Expr` node is converted multiple times, return the same
    /// `Value` so source-literal identity stays stable across re-evaluation.
    /// Only compound types (`List`, `DottedList`, `Vector`, `Str`) benefit
    /// from caching; scalars like `Int`, `Symbol`, `Char` already have
    /// identity-free representations.
    fn cached_source_literal_to_value(&mut self, expr: &Expr) -> Value {
        if expr.depends_on_reader_runtime_state() {
            return self.quote_to_runtime_value(expr);
        }
        let key = expr as *const Expr;
        if let Some(&cached) = self.source_literal_cache.get(&key) {
            return cached;
        }
        // For compound types, recursively cache children too
        let value = match expr {
            Expr::List(items) => {
                let quoted: Vec<Value> = items
                    .iter()
                    .map(|e| self.cached_source_literal_to_value(e))
                    .collect();
                Value::list(quoted)
            }
            Expr::DottedList(items, last) => {
                let head_vals: Vec<Value> = items
                    .iter()
                    .map(|e| self.cached_source_literal_to_value(e))
                    .collect();
                let tail_val = self.cached_source_literal_to_value(last);
                head_vals
                    .into_iter()
                    .rev()
                    .fold(tail_val, |acc, item| Value::cons(item, acc))
            }
            Expr::Vector(items) => {
                let vals: Vec<Value> = items
                    .iter()
                    .map(|e| self.cached_source_literal_to_value(e))
                    .collect();
                Value::vector(vals)
            }
            _ => self.quote_to_runtime_value(expr),
        };
        self.source_literal_cache.insert(key, value);
        value
    }
}

fn rewrite_wrong_arity_function_object(flow: Flow, name: &str) -> Flow {
    match flow {
        Flow::Signal(mut sig) => {
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.raw_data.is_none()
                && !sig.data.is_empty()
                && sig.data[0].as_symbol_name() == Some(name)
            {
                sig.data[0] = Value::subr(intern(name));
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

fn rewrite_wrong_arity_alias_function_object(flow: Flow, alias: &str, target: &str) -> Flow {
    match flow {
        Flow::Signal(mut sig) => {
            let target_is_payload = sig.data.first().is_some_and(|value| match value.kind() {
                ValueKind::Veclike(VecLikeType::Subr) => {
                    let id = value.as_subr_id().unwrap();
                    resolve_sym(id) == target || resolve_sym(id) == alias
                }
                _ => {
                    value.as_symbol_name() == Some(target) || value.as_symbol_name() == Some(alias)
                }
            });
            if sig.symbol_name() == "wrong-number-of-arguments"
                && !sig.data.is_empty()
                && target_is_payload
            {
                sig.data[0] = Value::symbol(alias);
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an Expr AST node to a Value (for quote).
pub fn quote_to_value(expr: &Expr) -> Value {
    match expr {
        Expr::Int(v) => Value::fixnum(*v),
        Expr::Float(v) => Value::make_float(*v),
        Expr::ReaderLoadFileName => Value::symbol("load-file-name"),
        Expr::Str(s) => Value::string(s.clone()),
        Expr::Char(c) => Value::char(*c),
        Expr::Keyword(id) => Value::from_kw_id(*id),
        Expr::Bool(true) => Value::T,
        Expr::Bool(false) => Value::NIL,
        Expr::Symbol(id) if resolve_sym(*id) == "nil" => Value::NIL,
        Expr::Symbol(id) if resolve_sym(*id) == "t" => Value::T,
        Expr::Symbol(id) => Value::from_sym_id(*id),
        Expr::List(items) => {
            let quoted = items.iter().map(quote_to_value).collect::<Vec<_>>();
            Value::list(quoted)
        }
        Expr::DottedList(items, last) => {
            let head_vals: Vec<Value> = items.iter().map(quote_to_value).collect();
            let tail_val = quote_to_value(last);
            head_vals
                .into_iter()
                .rev()
                .fold(tail_val, |acc, item| Value::cons(item, acc))
        }
        Expr::Vector(items) => {
            let vals = items.iter().map(quote_to_value).collect();
            Value::vector(vals)
        }
        Expr::OpaqueValueRef(idx) => OPAQUE_POOL.with(|pool| pool.borrow().get(*idx)),
    }
}

/// Collect all opaque value references from an Expr tree into a Vec.
///
/// With the OpaqueValuePool system, this is a no-op — the pool handles
/// GC tracing.  Kept for API compatibility.
pub(crate) fn collect_opaque_values(expr: &Expr, out: &mut Vec<Value>) {
    expr.collect_opaque_values(out);
}

fn format_startup_value(value: Option<&Value>) -> String {
    value
        .map(super::print::print_value)
        .unwrap_or_else(|| "<unbound>".to_string())
}

/// Convert a Value cons list to a Vec<Value> (for eval_sub arg passing).
fn value_list_to_values(list: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    let mut cursor = *list;
    while cursor.is_cons() {
        result.push(cursor.cons_car());
        cursor = cursor.cons_cdr();
    }
    result
}

/// Convert a Value cons list to Vec<Expr> for special form dispatch.
/// This is a SHALLOW conversion — each element becomes a single Expr node.
/// Used by eval_sub to interface with Expr-based special form handlers.
fn value_list_to_exprs(list: &Value) -> Vec<Expr> {
    let mut result = Vec::new();
    let mut cursor = *list;
    while cursor.is_cons() {
        result.push(value_to_expr(&cursor.cons_car()));
        cursor = cursor.cons_cdr();
    }
    result
}

/// Convert a Value back to an Expr (for macro expansion).
pub(crate) fn value_to_expr(value: &Value) -> Expr {
    match value.kind() {
        ValueKind::Nil => Expr::Symbol(intern("nil")),
        ValueKind::T => Expr::Symbol(intern("t")),
        ValueKind::Fixnum(n) => Expr::Int(n),
        ValueKind::Float => Expr::Float(value.as_float().unwrap()),
        ValueKind::Symbol(id) => Expr::Symbol(id),
        ValueKind::String => Expr::Str(value.as_str().unwrap().to_owned()),
        ValueKind::Cons => {
            if let Some(items) = list_to_vec(value) {
                Expr::List(items.iter().map(value_to_expr).collect())
            } else {
                // Improper list / dotted pair — traverse cons cells and
                // produce Expr::DottedList(proper_items, tail).
                let mut items = Vec::new();
                let mut cursor = *value;
                loop {
                    match cursor.kind() {
                        ValueKind::Cons => {
                            items.push(value_to_expr(&cursor.cons_car()));
                            cursor = cursor.cons_cdr();
                        }
                        _ => {
                            break Expr::DottedList(items, Box::new(value_to_expr(&cursor)));
                        }
                    }
                }
            }
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            Expr::Vector(items.iter().map(value_to_expr).collect())
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(*value)))
        }
        // Lambda, Macro, ByteCode, HashTable, Buffer, etc. — preserve as
        // opaque values so they survive the Value→Expr→Value round-trip
        // (e.g., closures embedded in defcustom backquote expansions).
        other => Expr::OpaqueValueRef(OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(*value))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "eval_test.rs"]
mod tests;
