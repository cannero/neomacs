//! Context — special forms, function application, and dispatch.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::OnceLock;

use smallvec::SmallVec;

use super::abbrev::AbbrevManager;
use super::advice::VariableWatcherList;
use super::autoload::AutoloadManager;
use super::bookmark::BookmarkManager;
use super::builtins;
use super::coding::CodingSystemManager;
use super::custom::CustomManager;
use super::doc::{STARTUP_VARIABLE_DOC_STRING_PROPERTIES, STARTUP_VARIABLE_DOC_STUBS};
use super::error::*;
use super::interactive::InteractiveRegistry;
use super::intern::{
    NameId, SymId, intern, intern_uninterned, is_canonical_id, is_keyword_id, resolve_name,
    resolve_sym, resolve_sym_metadata, symbol_name_id,
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
use crate::gc_trace::GcTrace;
use crate::tagged::header::{CLOSURE_ARGLIST, SubrDispatchKind};
use crate::window::FrameManager;

const EVAL_STACK_RED_ZONE: usize = 128 * 1024;
const EVAL_STACK_SEGMENT: usize = 2 * 1024 * 1024;
const STACK_GROWTH_PROBE_START_DEPTH: usize = 16;
const STACK_GROWTH_PROBE_INTERVAL: usize = 16;
/// Capacity of the per-Context cache mapping symbol → resolved call
/// target.  The cache is keyed by `function_epoch` and invalidated
/// whenever the obarray's function cells change.  GNU Emacs has no such
/// cache (its dispatcher walks the symbol's function cell directly per
/// call), but in NeoMacs's debug build a fast path that avoids
/// `resolve_sym`/`intern` lock acquisitions per call is a major win
/// for byte-compiler workloads.  4096 entries comfortably covers the
/// distinct functions called during batch-byte-compile so the cache
/// never thrashes once warmed.
const NAMED_CALL_CACHE_CAPACITY: usize = 4096;
const LEXENV_ASSQ_CACHE_CAPACITY: usize = 16;
const LEXENV_SPECIAL_CACHE_CAPACITY: usize = 16;
const GC_DEFAULT_THRESHOLD_BYTES: usize = 100_000 * std::mem::size_of::<usize>();
const GC_THRESHOLD_FLOOR_BYTES: usize = GC_DEFAULT_THRESHOLD_BYTES / 10;
const GC_HI_THRESHOLD_BYTES: usize = (i64::MAX as usize) / 2;
const GC_PERCENT_SCALE: u64 = 1_000_000;
pub(crate) const INTERNAL_COMPILER_FUNCTION_OVERRIDES: &str =
    "internal--compiler-function-overrides";

/// Static subr entry — lives in global table, not on the tagged heap.
/// Replaces the heap-allocated SubrObj for builtin function metadata.
#[derive(Clone)]
pub(crate) struct SubrEntry {
    pub(crate) function: Option<crate::tagged::header::SubrFn>,
    pub(crate) min_args: u16,
    pub(crate) max_args: Option<u16>,
    pub(crate) dispatch_kind: crate::tagged::header::SubrDispatchKind,
    pub(crate) name_id: crate::emacs_core::intern::NameId,
}

thread_local! {
    static GLOBAL_SUBR_TABLE: RefCell<HashMap<SymId, SubrEntry>> = RefCell::new(HashMap::new());

    /// Thread-local handle to the active `Context::quit_requested`
    /// atomic. Installed by `Context::setup_thread_locals`, read by
    /// leaf functions (e.g. the regex matcher) that need a cheap quit
    /// check without threading `&mut Context` through their signature.
    /// Mirrors the call site shape of GNU's `maybe_quit()` — reachable
    /// from anywhere without an explicit context pointer.
    static QUIT_REQUESTED_TLS: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { RefCell::new(None) };
}

/// Check whether a quit is pending without needing `&mut Context`.
/// The regex matcher calls this at jump/fail sites, mirroring GNU's
/// `regex-emacs.c:4901,5236`. When it returns `true`, the caller
/// should unwind its work so the next `maybe_quit()` poll can promote
/// the pending flag to a `quit` signal.
pub(crate) fn tls_quit_pending() -> bool {
    QUIT_REQUESTED_TLS.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    })
}

/// Register a subr entry in the global static table.
pub(crate) fn register_global_subr_entry(sym_id: SymId, entry: SubrEntry) {
    GLOBAL_SUBR_TABLE.with(|table| {
        table.borrow_mut().insert(sym_id, entry);
    });
}

/// Look up a subr entry by SymId.
pub(crate) fn lookup_global_subr_entry(sym_id: SymId) -> Option<SubrEntry> {
    GLOBAL_SUBR_TABLE.with(|table| {
        table.borrow().get(&sym_id).cloned()
    })
}

/// Access a subr entry by reference (avoids cloning).
pub(crate) fn with_global_subr_entry<R>(sym_id: SymId, f: impl FnOnce(&SubrEntry) -> R) -> Option<R> {
    GLOBAL_SUBR_TABLE.with(|table| {
        table.borrow().get(&sym_id).map(f)
    })
}

/// Clear all subr entries (used during heap reset).
pub(crate) fn clear_global_subr_table() {
    GLOBAL_SUBR_TABLE.with(|table| table.borrow_mut().clear());
}

/// Cached SymId for `internal--compiler-function-overrides`.
///
/// `compiler_function_overrides_active_in_obarray` is called from
/// `resolve_named_call_target_by_id` on every funcall.  The previous
/// implementation re-interned the string each call, which acquires the
/// global interner write lock — even after `parking_lot::RwLock`, that
/// is many extra atomic ops per call and shows up as the dominant cost
/// in debug-build batch-byte-compile profiles.  Cache the SymId once
/// and use the `_id`-suffixed obarray accessor that bypasses intern.
fn internal_compiler_function_overrides_sym() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern(INTERNAL_COMPILER_FUNCTION_OVERRIDES))
}

#[inline]
fn internal_make_interpreted_closure_function_symbol() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("internal-make-interpreted-closure-function"))
}

#[inline]
fn cconv_make_interpreted_closure_symbol() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("cconv-make-interpreted-closure"))
}

#[inline]
fn load_in_progress_symbol() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("load-in-progress"))
}

#[inline]
fn macroexpand_all_environment_symbol() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("macroexpand-all-environment"))
}

#[inline]
fn throw_symbol() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("throw"))
}

pub(crate) fn compiler_function_override_in_obarray(
    obarray: &Obarray,
    sym_id: SymId,
) -> Option<Value> {
    let overrides_sym = internal_compiler_function_overrides_sym();
    let mut cursor = obarray
        .symbol_value_id(overrides_sym)
        .copied()
        .unwrap_or(Value::NIL);
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if entry.is_cons() && entry.cons_car().as_symbol_id() == Some(sym_id) {
            return Some(entry.cons_cdr());
        }
    }
    None
}

pub(crate) fn compiler_function_overrides_active_in_obarray(obarray: &Obarray) -> bool {
    let overrides_sym = internal_compiler_function_overrides_sym();
    obarray
        .symbol_value_id(overrides_sym)
        .copied()
        .unwrap_or(Value::NIL)
        .is_cons()
}

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
    /// Lexical environment save/restore. Mirrors GNU's
    /// `specbind(Qinternal_interpreter_environment, ...)` which saves
    /// the current `Vinternal_interpreter_environment` on the specpdl.
    /// `unbind_to` restores `self.lexenv` to this value.
    LexicalEnv { old_lexenv: Value },
    /// Temporary GC root carried on the specpdl itself, mirroring GNU's
    /// use of specpdl-owned runtime state for unwind/helper temporaries.
    GcRoot { value: Value },
    /// Call frame for backtrace. Matches GNU SPECPDL_BACKTRACE.
    /// unbind_to discards these (no-op).
    ///
    /// `unevalled == true` mirrors GNU's `nargs == UNEVALLED` marker
    /// (eval.c:2585 for special forms). In that shape, `args` holds
    /// a single element: the original cons list of un-evaluated argument
    /// forms. The walker emits `(nil FUNC FORMS FLAGS)` for these
    /// (`backtrace_frame_apply`, eval.c:3993-3994).
    Backtrace {
        function: Value,
        args: LispArgVec,
        debug_on_exit: bool,
        unevalled: bool,
    },
    /// unwind-protect cleanup. Matches GNU SPECPDL_UNWIND.
    /// For interpreter: forms is a cons list, unbind_to calls sf_progn_value.
    /// For VM: forms is a callable (bytecode fn), unbind_to calls apply.
    UnwindProtect { forms: Value, lexenv: Value },
    /// save-excursion state. Matches GNU SPECPDL_UNWIND_EXCURSION.
    SaveExcursion { buffer_id: crate::buffer::BufferId, marker_id: u64 },
    /// save-current-buffer state. Matches GNU record_unwind_current_buffer.
    SaveCurrentBuffer { buffer_id: crate::buffer::BufferId },
    /// save-restriction state. Matches GNU SPECPDL_UNWIND with save_restriction_restore.
    SaveRestriction { state: crate::buffer::SavedRestrictionState },
    /// Placeholder. Matches GNU SPECPDL_NOP.
    Nop,
}


#[derive(Clone, Debug, Default)]
pub(crate) struct VmRootFrame {
    pub(crate) roots: LispArgVec,
}

impl VmRootFrame {
    fn new() -> Self {
        Self {
            roots: LispArgVec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingSafeFuncall {
    pub(crate) function: Value,
    pub(crate) args: LispArgVec,
}

pub(crate) type LispArgVec = SmallVec<[Value; 8]>;

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

fn runtime_tail_fingerprint(tail: &[Value]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut seen = std::collections::HashSet::new();
    tail.len().hash(&mut hasher);
    for (i, value) in tail.iter().enumerate() {
        i.hash(&mut hasher);
        value_fingerprint(*value, &mut hasher, 3, &mut seen);
    }
    hasher.finish()
}

fn value_fingerprint(
    value: Value,
    hasher: &mut impl std::hash::Hasher,
    depth: usize,
    seen: &mut std::collections::HashSet<usize>,
) {
    use std::hash::Hash;
    match value.kind() {
        ValueKind::Nil => 0u8.hash(hasher),
        ValueKind::T => 1u8.hash(hasher),
        ValueKind::Fixnum(n) => {
            2u8.hash(hasher);
            n.hash(hasher);
        }
        ValueKind::Float => {
            3u8.hash(hasher);
            value.as_float().unwrap().to_bits().hash(hasher);
        }
        ValueKind::Symbol(id) => {
            4u8.hash(hasher);
            id.0.hash(hasher);
        }
        ValueKind::String => {
            5u8.hash(hasher);
            let key = value.bits() as usize;
            if depth == 0 || !seen.insert(key) {
                key.hash(hasher);
                return;
            }
            let string = match value.as_lisp_string() {
                Some(string) => string,
                None => {
                    key.hash(hasher);
                    return;
                }
            };
            string.is_multibyte().hash(hasher);
            string.schars().hash(hasher);
            string.sbytes().hash(hasher);
            for byte in string.as_bytes().iter().take(32) {
                byte.hash(hasher);
            }
        }
        ValueKind::Cons => {
            6u8.hash(hasher);
            let key = value.bits() as usize;
            if depth == 0 || !seen.insert(key) {
                key.hash(hasher);
                return;
            }
            let mut cursor = value;
            let mut count = 0usize;
            while count < 4 && cursor.is_cons() {
                count.hash(hasher);
                value_fingerprint(cursor.cons_car(), hasher, depth - 1, seen);
                cursor = cursor.cons_cdr();
                count += 1;
            }
            value_fingerprint(cursor, hasher, depth - 1, seen);
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            7u8.hash(hasher);
            let key = value.bits() as usize;
            if depth == 0 || !seen.insert(key) {
                key.hash(hasher);
                return;
            }
            let items = value.as_vector_data().unwrap();
            items.len().hash(hasher);
            for item in items.iter().take(4) {
                value_fingerprint(*item, hasher, depth - 1, seen);
            }
        }
        ValueKind::Subr(sym_id) => {
            8u8.hash(hasher);
            sym_id.0.hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            8u8.hash(hasher);
            value.as_subr_id().unwrap().0.hash(hasher);
        }
        _ => {
            9u8.hash(hasher);
            value.bits().hash(hasher);
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
    params_value: Value,
    body_value: Value,
    iform_value: Value,
    env_shape: &[InterpretedClosureEnvEntry],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut seen = std::collections::HashSet::new();
    value_fingerprint(params_value, &mut hasher, 8, &mut seen);
    value_fingerprint(body_value, &mut hasher, 8, &mut seen);
    value_fingerprint(iform_value, &mut hasher, 8, &mut seen);
    env_shape.hash(&mut hasher);
    hasher.finish()
}

fn interpreted_closure_env_shape_hash(env_shape: &[InterpretedClosureEnvEntry]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
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
    Subr(Value),
    Void,
}

#[derive(Clone, Debug)]
struct NamedCallCacheEntry {
    function_epoch: u64,
    target: NamedCallTarget,
}

#[derive(Clone, Copy, Debug)]
struct LexenvAssqCacheEntry {
    lexenv_bits: usize,
    symbol: SymId,
    cell: Value,
}

#[derive(Clone, Debug, Default)]
struct LexenvAssqCache {
    entries: [Option<LexenvAssqCacheEntry>; LEXENV_ASSQ_CACHE_CAPACITY],
}

impl LexenvAssqCache {
    #[inline]
    fn slot(lexenv_bits: usize, sym_id: SymId) -> usize {
        let mixed = lexenv_bits.rotate_left(7) ^ (sym_id.0 as usize).wrapping_mul(0x9E37_79B1);
        mixed & (LEXENV_ASSQ_CACHE_CAPACITY - 1)
    }

    #[inline]
    fn find(&self, lexenv_bits: usize, sym_id: SymId) -> Option<Value> {
        let entry = self.entries[Self::slot(lexenv_bits, sym_id)]?;
        (entry.lexenv_bits == lexenv_bits && entry.symbol == sym_id).then_some(entry.cell)
    }

    #[inline]
    fn push(&mut self, entry: LexenvAssqCacheEntry) {
        let index = Self::slot(entry.lexenv_bits, entry.symbol);
        self.entries[index] = Some(entry);
    }
}

#[derive(Clone, Copy, Debug)]
struct LexenvSpecialCacheEntry {
    lexenv_bits: usize,
    symbol: SymId,
    declared_special: bool,
}

#[derive(Clone, Debug, Default)]
struct LexenvSpecialCache {
    entries: [Option<LexenvSpecialCacheEntry>; LEXENV_SPECIAL_CACHE_CAPACITY],
}

impl LexenvSpecialCache {
    #[inline]
    fn slot(lexenv_bits: usize, sym_id: SymId) -> usize {
        let mixed = lexenv_bits.rotate_left(7) ^ (sym_id.0 as usize).wrapping_mul(0x9E37_79B1);
        mixed & (LEXENV_SPECIAL_CACHE_CAPACITY - 1)
    }

    #[inline]
    fn find(&self, lexenv_bits: usize, sym_id: SymId) -> Option<bool> {
        let entry = self.entries[Self::slot(lexenv_bits, sym_id)]?;
        (entry.lexenv_bits == lexenv_bits && entry.symbol == sym_id)
            .then_some(entry.declared_special)
    }

    #[inline]
    fn push(&mut self, entry: LexenvSpecialCacheEntry) {
        let index = Self::slot(entry.lexenv_bits, entry.symbol);
        self.entries[index] = Some(entry);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum InterpretedClosureEnvEntry {
    TopLevelSentinel,
    Special(SymId),
    Binding(SymId),
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeMacroExpansionCacheEntry {
    expanded: Value,
    fingerprint: u64,
}

impl RuntimeMacroExpansionCacheEntry {
    fn new(expanded: Value, fingerprint: u64) -> Self {
        Self {
            expanded,
            fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MacroPerfCounter {
    calls: u64,
    total_us: u64,
    max_us: u64,
}

impl MacroPerfCounter {
    fn note_duration(&mut self, duration: std::time::Duration) {
        let elapsed_us = duration.as_micros() as u64;
        self.calls = self.calls.saturating_add(1);
        self.total_us = self.total_us.saturating_add(elapsed_us);
        self.max_us = self.max_us.max(elapsed_us);
    }

    fn summary(&self, label: &str) -> Option<String> {
        if self.calls == 0 {
            return None;
        }
        let avg_us = self.total_us / self.calls.max(1);
        Some(format!(
            "{label}=count:{} total:{:.2}ms avg:{:.3}ms max:{:.3}ms",
            self.calls,
            self.total_us as f64 / 1000.0,
            avg_us as f64 / 1000.0,
            self.max_us as f64 / 1000.0
        ))
    }
}

#[derive(Clone, Debug, Default)]
struct MacroPerfStats {
    scope_enter: MacroPerfCounter,
    scope_exit: MacroPerfCounter,
    macro_apply: MacroPerfCounter,
    cache_lookup: MacroPerfCounter,
    cache_store: MacroPerfCounter,
    expand_macro: MacroPerfCounter,
    eager_step1: MacroPerfCounter,
    eager_step3: MacroPerfCounter,
    eager_step4: MacroPerfCounter,
}

#[derive(Clone, Debug)]
struct InterpretedClosureTrimCacheEntry {
    params_value: Value,
    body_value: Value,
    iform_value: Value,
    env_shape: Vec<InterpretedClosureEnvEntry>,
    trimmed_params_value: Value,
    trimmed_body_value: Value,
    trimmed_env_template: Vec<InterpretedClosureEnvEntry>,
}

impl InterpretedClosureTrimCacheEntry {
    fn matches(
        &self,
        params_value: Value,
        body_value: Value,
        iform_value: Value,
        env_shape: &[InterpretedClosureEnvEntry],
    ) -> bool {
        equal_value(&self.params_value, &params_value, 0)
            && equal_value(&self.body_value, &body_value, 0)
            && equal_value(&self.iform_value, &iform_value, 0)
            && self.env_shape == env_shape
    }
}

#[derive(Clone, Debug)]
struct InterpretedClosureValueCacheEntry {
    source_function: Value,
    env_shape: Vec<InterpretedClosureEnvEntry>,
    trimmed_params_value: Value,
    trimmed_body_value: Value,
    trimmed_env_template: Vec<InterpretedClosureEnvEntry>,
}

impl InterpretedClosureValueCacheEntry {
    fn matches(&self, source_function: Value, env_shape: &[InterpretedClosureEnvEntry]) -> bool {
        equal_value(&self.source_function, &source_function, 0) && self.env_shape == env_shape
    }
}

fn value_from_symbol_id(sym_id: SymId) -> Value {
    if is_canonical_id(sym_id) {
        if sym_id == nil_symbol() {
            return Value::NIL;
        }
        if sym_id == t_symbol() {
            return Value::T;
        }
        if is_keyword_id(sym_id) {
            return Value::from_kw_id(sym_id);
        }
    }
    Value::from_sym_id(sym_id)
}

fn hidden_internal_interpreter_environment_symbol() -> SymId {
    static HIDDEN_SYMBOL: OnceLock<SymId> = OnceLock::new();
    *HIDDEN_SYMBOL.get_or_init(|| intern_uninterned("internal-interpreter-environment"))
}

fn lexical_binding_symbol() -> SymId {
    static SYMBOL: OnceLock<SymId> = OnceLock::new();
    *SYMBOL.get_or_init(|| intern("lexical-binding"))
}

fn nil_symbol() -> SymId {
    static SYMBOL: OnceLock<SymId> = OnceLock::new();
    *SYMBOL.get_or_init(|| intern("nil"))
}

fn t_symbol() -> SymId {
    static SYMBOL: OnceLock<SymId> = OnceLock::new();
    *SYMBOL.get_or_init(|| intern("t"))
}

fn macroexp_dynvars_symbol() -> SymId {
    static SYMBOL: OnceLock<SymId> = OnceLock::new();
    *SYMBOL.get_or_init(|| intern("macroexp--dynvars"))
}

macro_rules! cached_symbol_id {
    ($fn_name:ident, $name:literal) => {
        fn $fn_name() -> SymId {
            static SYMBOL: OnceLock<SymId> = OnceLock::new();
            *SYMBOL.get_or_init(|| intern($name))
        }
    };
}

cached_symbol_id!(quote_symbol, "quote");
cached_symbol_id!(function_symbol, "function");
cached_symbol_id!(let_symbol, "let");
cached_symbol_id!(let_star_symbol, "let*");
cached_symbol_id!(setq_symbol, "setq");
cached_symbol_id!(if_symbol, "if");
cached_symbol_id!(and_symbol, "and");
cached_symbol_id!(or_symbol, "or");
cached_symbol_id!(cond_symbol, "cond");
cached_symbol_id!(while_symbol, "while");
cached_symbol_id!(progn_symbol, "progn");
cached_symbol_id!(prog1_symbol, "prog1");
cached_symbol_id!(defvar_symbol, "defvar");
cached_symbol_id!(defconst_symbol, "defconst");
cached_symbol_id!(catch_symbol, "catch");
cached_symbol_id!(unwind_protect_symbol, "unwind-protect");
cached_symbol_id!(condition_case_symbol, "condition-case");
cached_symbol_id!(save_excursion_symbol, "save-excursion");
cached_symbol_id!(save_current_buffer_symbol, "save-current-buffer");
cached_symbol_id!(save_restriction_symbol, "save-restriction");
cached_symbol_id!(interactive_symbol_id, "interactive");
cached_symbol_id!(lambda_symbol, "lambda");
cached_symbol_id!(closure_symbol, "closure");
cached_symbol_id!(macro_symbol, "macro");
cached_symbol_id!(byte_code_literal_symbol, "byte-code-literal");
cached_symbol_id!(byte_code_symbol, "byte-code");
cached_symbol_id!(gc_cons_threshold_symbol, "gc-cons-threshold");
cached_symbol_id!(gc_cons_percentage_symbol, "gc-cons-percentage");
cached_symbol_id!(memory_full_symbol, "memory-full");
cached_symbol_id!(gc_elapsed_symbol, "gc-elapsed");
cached_symbol_id!(gcs_done_symbol, "gcs-done");

fn is_lambda_like_symbol_id(id: SymId) -> bool {
    id == lambda_symbol() || id == closure_symbol()
}

fn cons_head_symbol_id(value: &Value) -> Option<SymId> {
    if value.is_cons() {
        let car = value.cons_car();
        // Try bare symbol first, then transparently unwrap symbol-with-pos.
        car.as_symbol_id().or_else(|| {
            car.as_symbol_with_pos_sym()
                .and_then(|sym| sym.as_symbol_id())
        })
    } else {
        None
    }
}

struct CoreEvalSymbols {
    internal_interpreter_environment_symbol: SymId,
    quit_flag_symbol: SymId,
    inhibit_quit_symbol: SymId,
    throw_on_input_symbol: SymId,
    kill_emacs_symbol: SymId,
    noninteractive_symbol: SymId,
    symbols_with_pos_enabled_symbol: SymId,
    print_symbols_bare_symbol: SymId,
}

fn install_core_eval_symbols(obarray: &mut Obarray, reset_runtime_values: bool) -> CoreEvalSymbols {
    obarray.intern("internal-interpreter-environment");
    let internal_interpreter_environment_symbol = hidden_internal_interpreter_environment_symbol();
    obarray.set_symbol_value_id(internal_interpreter_environment_symbol, Value::NIL);
    obarray.make_special_id(internal_interpreter_environment_symbol);

    let quit_flag_symbol = intern("quit-flag");
    if reset_runtime_values {
        obarray.set_symbol_value_id(quit_flag_symbol, Value::NIL);
    }
    obarray.make_special_id(quit_flag_symbol);

    let inhibit_quit_symbol = intern("inhibit-quit");
    if reset_runtime_values {
        obarray.set_symbol_value_id(inhibit_quit_symbol, Value::NIL);
    }
    obarray.make_special_id(inhibit_quit_symbol);

    let throw_on_input_symbol = intern("throw-on-input");
    if reset_runtime_values {
        obarray.set_symbol_value_id(throw_on_input_symbol, Value::NIL);
    }
    obarray.make_special_id(throw_on_input_symbol);

    let kill_emacs_symbol = intern("kill-emacs");
    let noninteractive_symbol = intern("noninteractive");
    let symbols_with_pos_enabled_symbol = intern("symbols-with-pos-enabled");
    let print_symbols_bare_symbol = intern("print-symbols-bare");

    CoreEvalSymbols {
        internal_interpreter_environment_symbol,
        quit_flag_symbol,
        inhibit_quit_symbol,
        throw_on_input_symbol,
        kill_emacs_symbol,
        noninteractive_symbol,
        symbols_with_pos_enabled_symbol,
        print_symbols_bare_symbol,
    }
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
    // Use symbol_id to transparently handle symbol-with-pos wrappers.
    let sym_id = super::builtins::symbols::symbol_id(&feature).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), feature],
        )
    })?;
    let name = resolve_sym(sym_id).to_owned();
    if let Some(value) = subfeatures {
        obarray.put_property(&name, "subfeatures", value)?;
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
    super::syntax::collect_syntax_gc_roots(roots);
    super::casetab::collect_casetab_gc_roots(roots);
    super::category::collect_category_gc_roots(roots);
    super::value_reader::collect_value_reader_gc_roots(roots);
    super::terminal::pure::collect_terminal_gc_roots(roots);
    super::font::collect_font_gc_roots(roots);
    super::charset::collect_charset_gc_roots(roots);
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
    pub title: crate::heap_types::LispString,
    pub geometry_hints: crate::window::GuiFrameGeometryHints,
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
    pub family: Option<crate::heap_types::LispString>,
    pub registry: Option<crate::heap_types::LispString>,
    pub lang: Option<crate::heap_types::LispString>,
    pub weight: Option<FontWeight>,
    pub slant: Option<FontSlant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFontMatch {
    pub family: crate::heap_types::LispString,
    pub foundry: Option<crate::heap_types::LispString>,
    pub weight: FontWeight,
    pub slant: FontSlant,
    pub width: FontWidth,
    pub postscript_name: Option<crate::heap_types::LispString>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFrameFont {
    pub family: crate::heap_types::LispString,
    pub foundry: Option<crate::heap_types::LispString>,
    pub weight: FontWeight,
    pub slant: FontSlant,
    pub width: FontWidth,
    pub postscript_name: Option<crate::heap_types::LispString>,
    pub font_size_px: f32,
    pub char_width: f32,
    pub line_height: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFontSpecMatch {
    pub family: crate::heap_types::LispString,
    pub registry: Option<crate::heap_types::LispString>,
    pub weight: Option<FontWeight>,
    pub slant: Option<FontSlant>,
    pub width: Option<FontWidth>,
    pub spacing: Option<i32>,
    pub postscript_name: Option<crate::heap_types::LispString>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImageResolveSource {
    File(crate::heap_types::LispString),
    Data(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageResolveRequest {
    pub source: ImageResolveSource,
    pub max_width: u32,
    pub max_height: u32,
    pub fg_color: u32,
    pub bg_color: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedImage {
    pub image_id: u32,
    pub width: u32,
    pub height: u32,
}

pub trait DisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String>;
    fn resize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String>;
    fn set_gui_frame_geometry_hints(
        &mut self,
        _frame_id: crate::window::FrameId,
        _geometry_hints: crate::window::GuiFrameGeometryHints,
    ) -> Result<(), String> {
        Ok(())
    }
    fn set_gui_frame_title(
        &mut self,
        _frame_id: crate::window::FrameId,
        _title: crate::heap_types::LispString,
    ) -> Result<(), String> {
        Ok(())
    }
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
    fn resolve_frame_font(
        &mut self,
        _frame_id: crate::window::FrameId,
        _face: RuntimeFace,
    ) -> Result<Option<ResolvedFrameFont>, String> {
        Ok(None)
    }
    fn resolve_font_for_spec(
        &mut self,
        _request: FontSpecResolveRequest,
    ) -> Result<Option<ResolvedFontSpecMatch>, String> {
        Ok(None)
    }
    fn resolve_image(
        &self,
        _request: ImageResolveRequest,
    ) -> Result<Option<ResolvedImage>, String> {
        Ok(None)
    }
}

/// The Elisp evaluator.
///
/// # Safety: Send
/// Evaluator is inherently single-threaded (uses thread-local heap and caches).
/// # Safety: Send
/// Context is inherently single-threaded (uses thread-local heap and caches).
/// `neovm-worker` moves the Context to a worker thread inside
/// `Arc<Mutex<..>>`, which ensures exclusive access.
// SAFETY: Rc is !Send only because it uses non-atomic refcounting.
// Since Context is always used single-threaded (guarded by Mutex when
// transferred between threads), this is safe.
unsafe impl Send for Context {}

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
        bind_stack_len: usize,
    },
    VmConditionCase {
        resume_id: u64,
        target: u32,
        stack_len: usize,
        spec_depth: usize,
        bind_stack_len: usize,
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

/// Metadata for a single active bytecode frame in the contiguous `bc_buf`.
pub(crate) struct BcFrame {
    /// Index in `Context::bc_buf` where this frame's stack region starts.
    pub base: usize,
    /// The function value — keeps the bytecode object (and its constants)
    /// reachable by GC.
    pub fun: Value,
}

pub struct Context {
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
    noninteractive_symbol: SymId,
    noninteractive: bool,
    symbols_with_pos_enabled_symbol: SymId,
    /// When true, `symbolp`/`eq`/hash operations transparently unwrap
    /// symbol-with-pos objects. Bound to `t` by the byte-compiler.
    pub(crate) symbols_with_pos_enabled: bool,
    print_symbols_bare_symbol: SymId,
    /// When true, the printer outputs bare symbol names for symbol-with-pos.
    pub(crate) print_symbols_bare: bool,
    /// Features list (for require/provide).
    pub(crate) features: Vec<SymId>,
    /// Features currently being resolved through `require`.
    pub(crate) require_stack: Vec<SymId>,
    /// Files currently being loaded (mirrors `Vloads_in_progress` in lread.c).
    pub(crate) loads_in_progress: Vec<crate::heap_types::LispString>,
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
    /// Tree-sitter runtime manager — loaded grammars, parser state, node handles,
    /// and compiled queries backing `treesit-*` builtins.
    pub(crate) treesit: super::treesit::TreeSitterManager,
    /// Minibuffer runtime state — active minibuffer stack, prompt metadata, and history.
    pub(crate) minibuffers: MinibufferManager,
    /// Current echo-area message text, mirroring GNU `current-message`.
    pub(crate) current_message: Option<crate::heap_types::LispString>,
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
    /// Cross-thread quit signal. The input-bridge thread flips this to
    /// `true` when it observes a `quit-char` keystroke; the evaluator
    /// drains it from `maybe_quit` into `Vquit_flag` on its next poll.
    ///
    /// GNU handles this case with `sys_longjmp` from the signal or
    /// keystroke handler straight into `read_char`'s `setjmp` target
    /// (`keyboard.c:12738`, `keyboard.c:3812`). Rust can't do that
    /// across owned borrows, so we use an atomic flag and rely on
    /// `maybe_quit` polling from `eval_sub` / `Ffuncall` / the bytecode
    /// VM to pick it up.
    pub quit_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    /// Cached Lisp-visible GC tuning variables used on every safe point.
    ///
    /// GNU updates its low-level GC tuning state when the watched variables
    /// change, then keeps `maybe_gc` itself cheap.  Mirror that split here:
    /// refresh the cache on the mutation sites, and let safe points combine
    /// the cached values with current heap usage.
    gc_runtime_settings_cache: GcRuntimeSettingsCache,
    /// Active VM-local root frames. Mirrors GNU's model more closely than a
    /// single save/truncate side vector by keeping VM dynamic roots in explicit
    /// nested frames.
    vm_root_frames: Vec<VmRootFrame>,
    /// Contiguous bytecode stack buffer, matching GNU Emacs's bc_thread_state.
    /// All bytecode frames share this single buffer. GC scans it directly.
    pub(crate) bc_buf: Vec<Value>,
    /// Frame metadata for each active bytecode invocation.
    /// Each entry records where the frame's stack region starts in bc_buf
    /// and the function object (so GC can trace its constants).
    pub(crate) bc_frames: Vec<BcFrame>,
    /// Shared condition runtime mirror for active catch/condition handlers.
    pub(crate) condition_stack: Vec<ConditionFrame>,
    /// Stable identity source for VM resume targets stored in the shared
    /// condition runtime.
    next_resume_id: u64,
    /// GNU `pending_funcalls` equivalent for internal no-Lisp teardown paths.
    pub(crate) pending_safe_funcalls: Vec<PendingSafeFuncall>,
    /// Hot cache for named callable resolution in `funcall`/`apply`.
    /// Keyed by symbol id; entries are validated against the obarray's
    /// `function_epoch` so that any `defalias` / `fset` / autoload
    /// installation immediately invalidates stale lookups.
    named_call_cache: HashMap<SymId, NamedCallCacheEntry>,
    /// Small hot cache for GNU-shaped lexical env alist lookups.
    lexenv_assq_cache: RefCell<LexenvAssqCache>,
    /// Small hot cache for GNU-shaped lexical special declarations.
    lexenv_special_cache: RefCell<LexenvSpecialCache>,
    /// Nested depth of active macro-expansion scopes.
    macro_expansion_scope_depth: usize,
    /// Monotonic counter for Lisp-visible mutations performed while a macro
    /// expander is running. Eager-load caches use this to preserve GNU
    /// `eval-and-compile` side effects during replay.
    macro_expansion_mutation_epoch: u64,
    /// Diagnostic counters for macro expansion cache.
    pub(crate) macro_cache_hits: u64,
    pub(crate) macro_cache_misses: u64,
    pub(crate) macro_expand_total_us: u64,
    /// When true, skip cache lookups (still populate cache for timing).
    pub(crate) macro_cache_disabled: bool,
    /// Value-side eager-load macro cache used by `macroexpand`/
    /// `internal-macroexpand-for-load`.
    ///
    /// Keyed by macro identity plus a structural fingerprint of the runtime
    /// argument tail, so equivalent cons trees rebuilt from cached/bootstrap
    /// forms can reuse the same expansion.
    pub(crate) runtime_macro_expansion_cache:
        HashMap<(usize, usize, u64), RuntimeMacroExpansionCacheEntry>,
    /// When true, collect detailed timing counters for macro/eager-load paths.
    macro_perf_enabled: bool,
    macro_perf_stats: MacroPerfStats,
    /// Bootstrapped standard interpreted-closure filter function object.
    /// Used to memoize the GNU cconv closure-trimming path without changing
    /// semantics when users later rebind/advice the hook.
    interpreted_closure_filter_fn: Option<Value>,
    /// Cache of standard cconv interpreted-closure trimming results keyed by
    /// lambda syntax plus lexical-environment shape. The cached data stores
    /// only the selected env template and trimmed body, so captured values are
    /// always rebuilt from the current runtime environment on a hit.
    interpreted_closure_trim_cache: HashMap<u64, Vec<InterpretedClosureTrimCacheEntry>>,
    /// Value-native cache for runtime callable-cons lambda instantiation.
    /// Keyed by a shallow Value fingerprint of the source callable plus the
    /// lexical environment shape.
    interpreted_closure_value_cache: HashMap<(u64, u64), Vec<InterpretedClosureValueCacheEntry>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownRequest {
    pub exit_code: i32,
    pub restart: bool,
}

#[derive(Clone, Copy, Debug)]
struct GcRuntimeSettingsCache {
    gc_cons_threshold_bytes: usize,
    gc_cons_percentage_scaled: Option<u64>,
    memory_full: bool,
}

impl Default for GcRuntimeSettingsCache {
    fn default() -> Self {
        Self {
            gc_cons_threshold_bytes: GC_DEFAULT_THRESHOLD_BYTES,
            gc_cons_percentage_scaled: Some(100_000),
            memory_full: false,
        }
    }
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
    // Use symbol_id to transparently handle symbol-with-pos wrappers.
    let sym_id = super::builtins::symbols::symbol_id(&feature).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), feature],
        )
    })?;
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
        Some(v) if v.is_string() => runtime_string_value(v),
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

pub(crate) fn parse_eval_lexical_arg(arg: Option<Value>) -> Result<(bool, Option<Value>), Flow> {
    // GNU eval.c Feval (src/eval.c:2527):
    //   specbind(Qinternal_interpreter_environment,
    //            CONSP(lexical) || NILP(lexical) ? lexical : list_of_t);
    //
    // GNU ALWAYS specbinds — no case leaves the environment untouched.
    // We must always return Some(...) so the caller saves/restores lexenv.
    let Some(arg) = arg else {
        // No LEXICAL arg: clear lexical env (dynamic mode).
        return Ok((false, Some(Value::NIL)));
    };
    if arg.is_nil() {
        // LEXICAL is nil: clear lexical env (dynamic mode).
        return Ok((false, Some(Value::NIL)));
    }

    // Non-nil atom (like t) => lexical mode, env = (t)  [the list!]
    if !arg.is_cons() {
        return Ok((true, Some(Value::list(vec![Value::T]))));
    };

    // Cons (alist) => lexical mode, env = the alist
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
        .symbol_value_id(lexical_binding_symbol())
        .is_some_and(|v| v.is_truthy())
}

#[inline]
fn top_level_lexenv_sentinel() -> Value {
    Value::list(vec![Value::T])
}

#[inline]
fn lexenv_is_active(lexenv: Value) -> bool {
    !lexenv.is_nil()
}

#[inline]
fn is_top_level_lexenv_sentinel(lexenv: Value) -> bool {
    lexenv.is_cons() && lexenv.cons_car().is_t() && lexenv.cons_cdr().is_nil()
}

pub(crate) struct ActiveEvalLexicalArgState {
    specpdl_count: usize,
}

pub(crate) fn begin_eval_with_lexical_arg_in_state(
    _obarray: &mut Obarray,
    lexenv: &mut Value,
    specpdl: &mut Vec<SpecBinding>,
    lexical_arg: Option<Value>,
) -> Result<ActiveEvalLexicalArgState, Flow> {
    let (_use_lexical, lexenv_value) = parse_eval_lexical_arg(lexical_arg)?;
    // Mirrors GNU eval.c Feval:
    //   specbind(Qinternal_interpreter_environment, new_env);
    //   return unbind_to(count, eval_sub(form));
    //
    // We push a SpecBinding::LexicalEnv entry (saving the old lexenv)
    // and set lexenv to the new value. unbind_to restores it
    // automatically, providing unwind-safe cleanup on non-local exits.
    let specpdl_count = specpdl.len();
    if let Some(env) = lexenv_value {
        specpdl.push(SpecBinding::LexicalEnv {
            old_lexenv: *lexenv,
        });
        *lexenv = env;
    }
    Ok(ActiveEvalLexicalArgState { specpdl_count })
}

pub(crate) fn finish_eval_with_lexical_arg_in_state(
    _obarray: &mut Obarray,
    lexenv: &mut Value,
    specpdl: &mut Vec<SpecBinding>,
    state: ActiveEvalLexicalArgState,
) {
    // Mirrors GNU: unbind_to(count, result) which pops the
    // SpecBinding::LexicalEnv entry and restores self.lexenv.
    while specpdl.len() > state.specpdl_count {
        let binding = specpdl.pop().unwrap();
        match binding {
            SpecBinding::LexicalEnv { old_lexenv } => {
                *lexenv = old_lexenv;
            }
            other => {
                // Should not happen — begin only pushes LexicalEnv.
                // Put it back if it does.
                specpdl.push(other);
                break;
            }
        }
    }
}

pub(crate) struct ActiveLambdaCallState {
    specpdl_count: usize,
}

pub(crate) struct ActiveMacroExpansionScopeState {
    saved_specpdl_len: usize,
    old_lexical: bool,
    old_dynvars: Value,
}


#[derive(Clone, Copy, Debug)]
pub(crate) struct VmRootScopeState {
    pushed_vm_root_frame: bool,
    saved_vm_root_frame_len: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SpecpdlRootScopeState {
    saved_len: usize,
}

fn bind_lexical_value_rooted_in_specpdl(
    lexenv: &mut Value,
    specpdl: &mut Vec<SpecBinding>,
    sym: SymId,
    value: Value,
) {
    specpdl.push(SpecBinding::GcRoot { value });
    let binding = Value::make_cons(lexenv_binding_symbol_value(sym), value);
    match specpdl.last_mut() {
        Some(SpecBinding::GcRoot { value }) => *value = binding,
        other => panic!("expected temporary specpdl gc root entry, got {other:?}"),
    }
    *lexenv = Value::make_cons(binding, *lexenv);
    match specpdl.pop() {
        Some(SpecBinding::GcRoot { .. }) => {}
        other => panic!("expected temporary specpdl gc root entry, got {other:?}"),
    }
}

fn prepend_lexical_binding_in_specpdl_rooted_env(
    lexenv: &mut Value,
    specpdl: &mut Vec<SpecBinding>,
    env_root_index: usize,
    sym: SymId,
    value: Value,
) {
    specpdl.push(SpecBinding::GcRoot { value });
    let current_env = match specpdl.get(env_root_index) {
        Some(SpecBinding::GcRoot { value }) => *value,
        other => panic!("expected specpdl gc root entry for lexical env, got {other:?}"),
    };
    let binding = Value::make_cons(lexenv_binding_symbol_value(sym), value);
    match specpdl.last_mut() {
        Some(SpecBinding::GcRoot { value }) => *value = binding,
        other => panic!("expected temporary specpdl gc root entry, got {other:?}"),
    }
    let new_env = Value::make_cons(binding, current_env);
    match specpdl.get_mut(env_root_index) {
        Some(SpecBinding::GcRoot { value }) => *value = new_env,
        other => panic!("expected mutable specpdl gc root entry for lexical env, got {other:?}"),
    }
    *lexenv = new_env;
    match specpdl.pop() {
        Some(SpecBinding::GcRoot { .. }) => {}
        other => panic!("expected temporary specpdl gc root entry, got {other:?}"),
    }
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
    params: &LambdaParams,
    env: Option<Value>,
    args: &[Value],
) -> Result<ActiveLambdaCallState, Flow> {
    if args.len() < params.min_arity() {
        tracing::warn!(
            "wrong-number-of-arguments (lambda call too few): got {} args, min={}, params={:?}",
            args.len(),
            params.min_arity(),
            params,
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

    let specpdl_count = specpdl.len();

    if let Some(env) = env {
        // Debug: detect malformed env (bare t instead of list (t))
        if env.is_t() {
            tracing::error!(
                "Lambda called with env=t (should be (t))! params={:?}",
                params
            );
        }
        let old = std::mem::replace(lexenv, env);
        // Mirrors GNU funcall_lambda:
        //   specbind(Qinternal_interpreter_environment, lexenv);
        specpdl.push(SpecBinding::LexicalEnv { old_lexenv: old });

        let env_root_index = specpdl.len();
        specpdl.push(SpecBinding::GcRoot { value: env });

        let mut arg_idx = 0;
        for param in &params.required {
            prepend_lexical_binding_in_specpdl_rooted_env(
                lexenv,
                specpdl,
                env_root_index,
                *param,
                args[arg_idx],
            );
            arg_idx += 1;
        }
        for param in &params.optional {
            if arg_idx < args.len() {
                prepend_lexical_binding_in_specpdl_rooted_env(
                    lexenv,
                    specpdl,
                    env_root_index,
                    *param,
                    args[arg_idx],
                );
                arg_idx += 1;
            } else {
                prepend_lexical_binding_in_specpdl_rooted_env(
                    lexenv,
                    specpdl,
                    env_root_index,
                    *param,
                    Value::NIL,
                );
            }
        }
        if let Some(rest_name) = params.rest {
            let rest_value = Value::list_from_slice(&args[arg_idx..]);
            prepend_lexical_binding_in_specpdl_rooted_env(
                lexenv,
                specpdl,
                env_root_index,
                rest_name,
                rest_value,
            );
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
            let rest_value = Value::list_from_slice(&args[arg_idx..]);
            specbind_in_state(obarray, specpdl, rest_name, rest_value);
        }
    }

    // GNU never writes `lexical-binding` during lambda/closure calls.
    // The closure's captured env is installed in self.lexenv (above),
    // which is the single source of truth for "is lexical mode active?"
    // via lexical_binding() -> !self.lexenv.is_nil().

    Ok(ActiveLambdaCallState {
        specpdl_count,
    })
}

fn finish_lambda_call_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    lexenv: &mut Value,
    state: ActiveLambdaCallState,
) {
    // Unwind all specpdl entries back to the count saved at begin.
    // For lexical closures, this pops the SpecBinding::LexicalEnv
    // entry (restoring self.lexenv) plus any dynamic bindings.
    // For dynamic lambdas, this pops the specbind entries.
    // Mirrors GNU: unbind_to(count, val) in funcall_lambda.
    while specpdl.len() > state.specpdl_count {
        let binding = specpdl.pop().unwrap();
        match binding {
            SpecBinding::LexicalEnv { old_lexenv } => {
                *lexenv = old_lexenv;
            }
            SpecBinding::Let { sym_id, old_value } => match old_value {
                Some(val) => obarray.set_symbol_value_id(sym_id, val),
                None => obarray.makunbound_id(sym_id),
            },
            SpecBinding::GcRoot { .. } => {}
            SpecBinding::Backtrace { .. } => {}
            other => {
                // LetLocal/LetDefault shouldn't appear here in the
                // standalone path, but handle gracefully.
                specpdl.push(other);
                break;
            }
        }
    }
}

fn begin_macro_expansion_scope_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    buffers: &mut BufferManager,
    custom: &CustomManager,
    lexenv: Value,
) -> ActiveMacroExpansionScopeState {
    let nil_symbol = nil_symbol();
    let t_symbol = t_symbol();
    let saved_specpdl_len = specpdl.len();
    let old_lexical = obarray
        .symbol_value_id(lexical_binding_symbol())
        .is_some_and(|value| value.is_truthy());
    let old_dynvars = obarray
        .symbol_value_id(macroexp_dynvars_symbol())
        .cloned()
        .unwrap_or(Value::NIL);

    let dynvars_root_index = specpdl.len();
    specpdl.push(SpecBinding::GcRoot { value: old_dynvars });
    let mut dynvars = old_dynvars;
    for sym in lexenv_bare_symbols(lexenv) {
        if sym == t_symbol || sym == nil_symbol {
            continue;
        }
        dynvars = Value::cons(Value::from_sym_id(sym), dynvars);
        match specpdl.get_mut(dynvars_root_index) {
            Some(SpecBinding::GcRoot { value }) => *value = dynvars,
            other => panic!("expected macro-expansion dynvars gc root, got {other:?}"),
        }
    }
    let specpdl_dynvars: Vec<SymId> = specpdl
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            SpecBinding::Let { sym_id, .. }
            | SpecBinding::LetLocal { sym_id, .. }
            | SpecBinding::LetDefault { sym_id, .. } => Some(*sym_id),
            SpecBinding::LexicalEnv { .. }
            | SpecBinding::GcRoot { .. }
            | SpecBinding::Backtrace { .. }
            | SpecBinding::Nop
            | SpecBinding::UnwindProtect { .. }
            | SpecBinding::SaveExcursion { .. }
            | SpecBinding::SaveCurrentBuffer { .. }
            | SpecBinding::SaveRestriction { .. } => None,
        })
        .collect();
    for sym_id in specpdl_dynvars {
        if sym_id == t_symbol || sym_id == nil_symbol {
            continue;
        }
        dynvars = Value::cons(Value::from_sym_id(sym_id), dynvars);
        match specpdl.get_mut(dynvars_root_index) {
            Some(SpecBinding::GcRoot { value }) => *value = dynvars,
            other => panic!("expected macro-expansion dynvars gc root, got {other:?}"),
        }
    }

    obarray.set_symbol_value_id(lexical_binding_symbol(), Value::bool_val(!lexenv.is_nil()));
    set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        macroexp_dynvars_symbol(),
        dynvars,
    );

    ActiveMacroExpansionScopeState {
        saved_specpdl_len,
        old_lexical,
        old_dynvars,
    }
}

fn finish_macro_expansion_scope_in_state(
    obarray: &mut Obarray,
    specpdl: &mut Vec<SpecBinding>,
    buffers: &mut BufferManager,
    custom: &CustomManager,
    state: ActiveMacroExpansionScopeState,
) {
    set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        macroexp_dynvars_symbol(),
        state.old_dynvars,
    );
    obarray.set_symbol_value_id(lexical_binding_symbol(), Value::bool_val(state.old_lexical));
    specpdl.truncate(state.saved_specpdl_len);
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    #[inline]
    pub(crate) fn subr_dispatch_kind(&self, sym_id: SymId) -> Option<SubrDispatchKind> {
        lookup_global_subr_entry(sym_id).map(|e| e.dispatch_kind)
    }

    #[inline]
    pub(crate) fn subr_dispatch_kind_or_compat(&self, sym_id: SymId) -> SubrDispatchKind {
        self.subr_dispatch_kind(sym_id)
            .unwrap_or_else(|| super::subr_info::compat_subr_dispatch_kind(resolve_sym(sym_id)))
    }

    #[inline]
    fn subr_is_special_form_id(&self, sym_id: SymId) -> bool {
        self.subr_dispatch_kind_or_compat(sym_id) == SubrDispatchKind::SpecialForm
    }

    #[inline]
    fn subr_is_context_callable_id(&self, sym_id: SymId) -> bool {
        self.subr_dispatch_kind_or_compat(sym_id) == SubrDispatchKind::ContextCallable
    }

    #[inline]
    fn has_registered_subr(&self, sym_id: SymId) -> bool {
        lookup_global_subr_entry(sym_id)
            .is_some_and(|e| e.function.is_some())
    }

    pub fn new() -> Self {
        let mut ctx = Self::new_inner(true);
        ctx.initialize_gc_stack_bottom();
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
        ev.condition_stack.clear();
        ev.next_resume_id = 1;
        ev.named_call_cache.clear();

        ev.runtime_macro_expansion_cache.clear();
        ev.macro_cache_hits = 0;
        ev.macro_cache_misses = 0;
        ev.macro_expand_total_us = 0;
        ev.macro_cache_disabled = false;
        ev.macro_perf_enabled = std::env::var_os("NEOVM_TRACE_MACRO_PERF").is_some();
        ev.macro_perf_stats = MacroPerfStats::default();
        ev.interpreted_closure_filter_fn = None;
        ev.interpreted_closure_trim_cache.clear();
        ev.materialize_public_evaluator_function_cells();
        ev.finish_runtime_activation(false);
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
        if sig.symbol == self.kill_emacs_symbol {
            return Err(Flow::Signal(sig));
        }
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

                    let specpdl_root_scope = self.save_specpdl_roots();
                    for value in &sig.data {
                        self.push_specpdl_root(*value);
                    }
                    if let Some(raw) = &sig.raw_data {
                        self.push_specpdl_root(*raw);
                    }

                    self.push_condition_frame(ConditionFrame::SkipConditions {
                        remaining: seen_condition_entries + mute_span,
                    });

                    let handler_result = self.apply(handler, vec![make_signal_binding_value(&sig)]);

                    match handler_result {
                        Ok(_) => {
                            self.pop_condition_frame();
                            self.restore_specpdl_roots(specpdl_root_scope);
                            continue;
                        }
                        Err(Flow::Signal(next_sig)) => {
                            let dispatched = self.dispatch_signal_if_needed(next_sig);
                            self.pop_condition_frame();
                            self.restore_specpdl_roots(specpdl_root_scope);
                            return dispatched;
                        }
                        Err(flow @ Flow::Throw { .. }) => {
                            self.pop_condition_frame();
                            self.restore_specpdl_roots(specpdl_root_scope);
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
            if entry.is_string() {
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
        // Create the heap and set thread-locals so tagged constructors work
        // during evaluator initialization.
        let mut tagged_heap = Box::new(crate::tagged::gc::TaggedHeap::new());
        crate::tagged::gc::set_tagged_heap(&mut tagged_heap);

        // Clear any caches that hold heap-allocated Values (tagged pointers) from a
        // previous heap. Critical for test isolation when multiple Contexts
        // are created sequentially on the same thread.
        if reset_thread_locals {
            super::pdump::runtime::reset_runtime_for_new_heap(
                super::pdump::runtime::HeapResetMode::FreshContext,
            );
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
        // GNU Emacs uses unibyte for default-directory during dump because
        // the locale isn't set up yet (see init_buffer in buffer.c).
        obarray.set_symbol_value(
            "default-directory",
            Value::unibyte_string(default_directory.clone()),
        );
        obarray.set_symbol_value(
            "command-line-default-directory",
            Value::unibyte_string(default_directory),
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
        obarray.set_symbol_value(
            "pdumper-fingerprint",
            Value::string(crate::emacs_core::pdump::fingerprint_hex()),
        );
        obarray.make_special("pdumper-fingerprint");
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
        // GNU `keyboard.c:14070` (`DEFVAR_LISP ("delayed-warnings-hook", ...)`)
        // — Lisp callers `display-warning` etc. expect this symbol
        // to exist as a hook list. Keyboard audit Finding 17 in
        // `drafts/keyboard-command-loop-audit.md`.
        obarray.set_symbol_value("delayed-warnings-hook", Value::NIL);
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
        obarray.set_symbol_value_id(lexical_binding_symbol(), Value::NIL);
        obarray.set_symbol_value("load-prefer-newer", Value::NIL);
        obarray.set_symbol_value("load-file-name", Value::NIL);
        obarray.make_special("load-file-name");
        obarray.set_symbol_value("noninteractive", Value::T);
        obarray.set_symbol_value("inhibit-quit", Value::NIL);
        obarray.set_symbol_value("symbols-with-pos-enabled", Value::NIL);
        obarray.make_special("symbols-with-pos-enabled");
        obarray.set_symbol_value("print-symbols-bare", Value::NIL);
        obarray.make_special("print-symbols-bare");
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
            "treesit-auto-install-grammar",
            "treesit-enabled-modes",
            "treesit-language-remap-alist",
            "treesit-language-source-alist",
            "treesit-load-name-override-list",
            "treesit-languages-require-line-column-tracking",
            "treesit-major-mode-remap-alist",
            "treesit-thing-settings",
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
            // Mouse pointer shapes — GNU defines these in
            // src/xfns.c (and parallel files w32fns.c, pgtkfns.c,
            // haikufns.c, androidfns.c) as integer Lisp_Object
            // variables that hold X cursor font codes. neomacs has
            // no native window-system bindings for these yet, so
            // they default to nil. Cursor audit Finding 9 in
            // drafts/cursor-audit.md flagged the symbols as
            // missing entirely; Lisp code that tried
            // (setq x-pointer-shape ...) hit void-variable.
            "x-pointer-shape",
            "x-nontext-pointer-shape",
            "x-mode-pointer-shape",
            "x-sensitive-text-pointer-shape",
            "x-hourglass-pointer-shape",
            "x-window-horizontal-drag-cursor",
            "x-window-vertical-drag-cursor",
            "x-window-left-edge-cursor",
            "x-window-top-left-corner-cursor",
            "x-window-top-edge-cursor",
            "x-window-top-right-corner-cursor",
            "x-window-right-edge-cursor",
            "x-window-bottom-right-corner-cursor",
            "x-window-bottom-edge-cursor",
            "x-window-bottom-left-corner-cursor",
            "x-cursor-fore-pixel",
        ] {
            obarray.set_symbol_value(name, Value::NIL);
        }
        // GNU `frame.c` initializes these global minor-mode variables in C:
        //   Vmenu_bar_mode = Qt
        //   Vtool_bar_mode = Qt   (when built with window-system support)
        // neomacs is a window-system-capable build, so match GNU's defaults
        // instead of starting graphical sessions with both modes forced off.
        obarray.set_symbol_value("menu-bar-mode", Value::T);
        obarray.set_symbol_value("tool-bar-mode", Value::T);
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
        // GNU `keyboard.c:14127`:
        //   DEFVAR_LISP ("prefix-help-command", Vprefix_help_command, ...);
        //   Vprefix_help_command = intern_c_string ("describe-prefix-bindings");
        // The default is consulted by `read_key_sequence` when the
        // help-char fires after a prefix. Keyboard audit Finding 5
        // in `drafts/keyboard-command-loop-audit.md`.
        obarray.set_symbol_value(
            "prefix-help-command",
            Value::symbol("describe-prefix-bindings"),
        );
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
        let core_eval_symbols = install_core_eval_symbols(&mut obarray, true);
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
        // Do NOT hardcode completion-styles-alist here.
        // GNU defines it via (defvar completion-styles-alist ...)
        // in lisp/minibuffer.el:1158 with all 8 styles including
        // flex, substring, initials, shorthand. defvar only sets
        // the value when the symbol is void, so pre-setting it
        // here would shadow the Lisp definition and lose styles
        // like flex — breaking fido-vertical-mode which requires
        // the flex completion style.
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
        // GNU `keyboard.c:14147` initialises `Vinput_method_function`
        // to `Qlist` as a placeholder, but that is a C-side
        // representation of "no function". The observable default
        // at the Lisp level is `nil` (checked via `(null
        // input-method-function)` in `lisp/international/mule.el`
        // and countless input-method packages). Keyboard audit
        // Finding 10 in `drafts/keyboard-command-loop-audit.md`.
        obarray.set_symbol_value("input-method-function", Value::NIL);
        obarray.set_symbol_value("input-method-highlight-flag", Value::T);
        obarray.set_symbol_value("input-method-history", Value::NIL);
        // input-method-previous-message is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("input-method-use-echo-area", Value::NIL);
        obarray.set_symbol_value("input-method-verbose-flag", Value::symbol("default"));
        obarray.set_symbol_value("unread-command-events", Value::NIL);
        // GNU Emacs seeds core startup vars with integer
        // `variable-documentation` offsets in the DOC table.
        for &(name, _) in STARTUP_VARIABLE_DOC_STUBS {
            obarray.put_property(name, "variable-documentation", Value::fixnum(0))
                .expect("startup variable-documentation plist should always be valid");
        }
        // Some startup docs are string-valued in GNU Emacs (not integer offsets).
        for &(name, doc) in STARTUP_VARIABLE_DOC_STRING_PROPERTIES {
            obarray.put_property(name, "variable-documentation", Value::string(doc))
                .expect("startup variable-documentation plist should always be valid");
        }

        // Bootstrap primitive function cells that GNU `simple.el` references
        // before its own Elisp defs overwrite them. Without these placeholders,
        // loaded GNU bytecode can capture `nil` for forward/runtime calls into
        // Builtin function cells are set by defsubr() during init_builtins().
        for name in ["mark-marker", "region-beginning", "region-end"] {
            obarray.set_symbol_function(name, Value::subr_from_sym_id(intern(name)));
        }

        // `word-at-point` is defined in GNU Emacs Lisp by `thingatpt.el`,
        // not as a startup builtin.
        obarray.clear_function_silent("word-at-point");

        // Mark standard variables as special (dynamically bound)
        for name in &[
            "debug-on-error",
            "debugger",
            // "lexical-binding" — now registered via defvar_per_buffer!
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

        // `case-fold-search` is DEFVAR_LISP + Fmake_variable_buffer_local
        // in GNU `buffer.c:5971-5975`. Install it as a LOCALIZED symbol
        // with `local_if_set = 1` at init time so reads/writes route
        // through the BLV + local_var_alist path instead of the legacy
        // `BufferLocals::lisp_bindings` fallback. Default is `t`.
        {
            let id = crate::emacs_core::intern::intern("case-fold-search");
            obarray.set_symbol_value("case-fold-search", Value::T);
            obarray.make_symbol_localized(id, Value::T);
            obarray.set_blv_local_if_set(id, true);
        }

        // `indent-tabs-mode` is DEFVAR_BOOL + make-variable-buffer-local
        // (bindings.el:1032). GNU's DEFVAR_BOOL installs a C-backed
        // forwarder; NeoMacs stores it as a plain Lisp value and
        // then hoists it to LOCALIZED at init. Default is `t`
        // (matches `init_indent_vars`).
        {
            let id = crate::emacs_core::intern::intern("indent-tabs-mode");
            obarray.make_symbol_localized(id, Value::T);
            obarray.set_blv_local_if_set(id, true);
        }

        super::textprop::init_textprop_vars(&mut obarray, &mut custom);
        super::syntax::init_syntax_vars(&mut obarray, &mut custom);
        // Register all DEFVAR_PER_BUFFER variables from GNU Emacs buffer.c.
        // These are C-level buffer-local variables that must exist before
        // any .el file loads.  Default values match init_buffer_once().
        macro_rules! defvar_per_buffer {
            ($name:expr, $val:expr) => {
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
                // GNU Emacs uses make_unibyte_string for default-directory
                // because the locale isn't set up yet during dump.  loadup.el
                // checks (multibyte-string-p default-directory) and errors
                // if it's multibyte.
                defvar_per_buffer!("default-directory", Value::unibyte_string(cwd));
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

            // Lexical binding (GNU buffer.c DEFVAR_PER_BUFFER).
            // Default is nil; each file sets it from -*- cookie.
            defvar_per_buffer!("lexical-binding", Value::NIL);

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

            // Phase 10B/C: install BUFFER_OBJFWD descriptors for
            // every entry in BUFFER_SLOT_INFO. After this point
            // each of these symbols has redirect=Forwarded with a
            // descriptor that resolves reads/writes to
            // `Buffer::slots[offset]`. The earlier
            // `defvar_per_buffer!` left them as LOCALIZED; we
            // overwrite that with the FORWARDED tag here so the
            // VM lookup/assign hot path takes the slot fast path.
            //
            // Mirrors GNU's `defvar_per_buffer` in `buffer.c`,
            // which always uses BUFFER_OBJFWD for these C-side
            // BVAR slots (`buffer.h:319-329`).
            {
                use crate::buffer::buffer::BUFFER_SLOT_INFO;
                use crate::emacs_core::forward::alloc_buffer_objfwd;
                use crate::emacs_core::intern::intern;

                for info in BUFFER_SLOT_INFO {
                    if !info.install_as_forwarder {
                        // Internal BVAR-only slot (syntax-table /
                        // category-table / case-table). Mirrors GNU's
                        // handling of `syntax_table_` etc. which
                        // occupy BVAR slot positions but are not
                        // DEFVAR_PER_BUFFER'd. Reads/writes happen
                        // exclusively through dedicated builtins.
                        continue;
                    }
                    let id = intern(info.name);
                    let predicate = if info.predicate.is_empty() {
                        intern("null")
                    } else {
                        intern(info.predicate)
                    };
                    let fwd = alloc_buffer_objfwd(
                        info.offset as u16,
                        info.local_flags_idx,
                        predicate,
                        info.default.to_value(),
                    );
                    obarray.install_buffer_objfwd(id, fwd);
                }
            }
        }

        // -----------------------------------------------------------------
        // C-level DEFVAR registrations: mirrors GNU's syms_of_*() functions.
        //
        // GNU Emacs declares hundreds of C-backed Lisp variables via
        // DEFVAR_LISP / DEFVAR_BOOL / DEFVAR_INT in its src/*.c files.
        // Each becomes a globally-visible symbol with a default value.
        // Elisp code reads/writes them freely; many are let-bound in
        // standard .el files during bootstrap and normal operation.
        //
        // If a variable is declared via DEFVAR in GNU's C code, it
        // MUST be registered here. Otherwise any elisp code that
        // reads or let-binds it will get void-variable.
        // -----------------------------------------------------------------

        // --- src/search.c: syms_of_search ---
        // DEFVAR_LISP, default nil. Let-bound extensively in subr.el,
        // custom.el, widget.el, mule.el, etc. to freeze match data
        // during internal string-match calls.
        obarray.set_symbol_value("inhibit-changing-match-data", Value::NIL);
        obarray.make_special("inhibit-changing-match-data");

        // --- src/casefiddle.c: syms_of_casefiddle ---
        // DEFVAR_BOOL + Fmake_variable_buffer_local, default 0 (nil).
        // Checked by case-conversion functions. Buffer-local via
        // make-variable-buffer-local (NOT defvar_per_buffer).
        {
            let id = crate::emacs_core::intern::intern("case-symbols-as-words");
            obarray.set_symbol_value("case-symbols-as-words", Value::NIL);
            obarray.make_symbol_localized(id, Value::NIL);
            obarray.set_blv_local_if_set(id, true);
        }

        // --- src/emacs.c: syms_of_emacs ---
        // DEFVAR_LISP, default nil. Run by kill-emacs.
        obarray.set_symbol_value("kill-emacs-hook", Value::NIL);
        obarray.make_special("kill-emacs-hook");

        let mut command_loop = crate::keyboard::CommandLoop::new();
        command_loop
            .keyboard
            .set_terminal_translation_maps(input_decode_map, local_function_key_map);
        let noninteractive = obarray
            .symbol_value_id(core_eval_symbols.noninteractive_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();
        let symbols_with_pos_enabled = obarray
            .symbol_value_id(core_eval_symbols.symbols_with_pos_enabled_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();
        let print_symbols_bare = obarray
            .symbol_value_id(core_eval_symbols.print_symbols_bare_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();

        let mut ev = Self {
            tagged_heap,
            obarray,
            specpdl: Vec::new(),
            lexenv: Value::NIL,
            internal_interpreter_environment_symbol: core_eval_symbols
                .internal_interpreter_environment_symbol,
            quit_flag_symbol: core_eval_symbols.quit_flag_symbol,
            inhibit_quit_symbol: core_eval_symbols.inhibit_quit_symbol,
            throw_on_input_symbol: core_eval_symbols.throw_on_input_symbol,
            kill_emacs_symbol: core_eval_symbols.kill_emacs_symbol,
            noninteractive_symbol: core_eval_symbols.noninteractive_symbol,
            noninteractive,
            symbols_with_pos_enabled_symbol: core_eval_symbols.symbols_with_pos_enabled_symbol,
            symbols_with_pos_enabled,
            print_symbols_bare_symbol: core_eval_symbols.print_symbols_bare_symbol,
            print_symbols_bare,
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
            treesit: super::treesit::TreeSitterManager::new(),
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
            quit_requested: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
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
            gc_runtime_settings_cache: GcRuntimeSettingsCache::default(),
            vm_root_frames: Vec::new(),
            bc_buf: Vec::with_capacity(4096),
            bc_frames: Vec::new(),
            condition_stack: Vec::new(),
            next_resume_id: 1,
            pending_safe_funcalls: Vec::new(),
            named_call_cache: HashMap::with_capacity(NAMED_CALL_CACHE_CAPACITY),
            lexenv_assq_cache: RefCell::new(LexenvAssqCache::default()),
            lexenv_special_cache: RefCell::new(LexenvSpecialCache::default()),

            macro_expansion_scope_depth: 0,
            macro_expansion_mutation_epoch: 0,
            macro_cache_hits: 0,
            macro_cache_misses: 0,
            macro_expand_total_us: 0,
            macro_cache_disabled: false,
            runtime_macro_expansion_cache: HashMap::new(),
            macro_perf_enabled: std::env::var_os("NEOVM_TRACE_MACRO_PERF").is_some(),
            macro_perf_stats: MacroPerfStats::default(),
            interpreted_closure_filter_fn: None,
            interpreted_closure_trim_cache: HashMap::new(),
            interpreted_closure_value_cache: HashMap::new(),
        };
        ev.finish_runtime_activation(false);
        ev
    }

    // -----------------------------------------------------------------------
    // pdump reconstruction
    // -----------------------------------------------------------------------

    /// Reconstruct an Context from pdump data.
    ///
    /// Thread-local heap pointers and caches must already be set by the caller
    /// before calling this.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_dump(
        tagged_heap: Box<crate::tagged::gc::TaggedHeap>,
        obarray: Obarray,
        lexenv: Value,
        features: Vec<SymId>,
        require_stack: Vec<SymId>,
        loads_in_progress: Vec<crate::heap_types::LispString>,
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
        let core_eval_symbols = install_core_eval_symbols(&mut obarray, false);
        let mut tagged_heap = tagged_heap;
        crate::tagged::gc::set_tagged_heap(&mut tagged_heap);
        let noninteractive = obarray
            .symbol_value_id(core_eval_symbols.noninteractive_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();
        let symbols_with_pos_enabled = obarray
            .symbol_value_id(core_eval_symbols.symbols_with_pos_enabled_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();
        let print_symbols_bare = obarray
            .symbol_value_id(core_eval_symbols.print_symbols_bare_symbol)
            .copied()
            .unwrap_or(Value::NIL)
            .is_truthy();

        let mut ev = Self {
            tagged_heap,
            obarray,
            specpdl: Vec::new(),
            lexenv,
            internal_interpreter_environment_symbol: core_eval_symbols
                .internal_interpreter_environment_symbol,
            quit_flag_symbol: core_eval_symbols.quit_flag_symbol,
            inhibit_quit_symbol: core_eval_symbols.inhibit_quit_symbol,
            throw_on_input_symbol: core_eval_symbols.throw_on_input_symbol,
            kill_emacs_symbol: core_eval_symbols.kill_emacs_symbol,
            noninteractive_symbol: core_eval_symbols.noninteractive_symbol,
            noninteractive,
            symbols_with_pos_enabled_symbol: core_eval_symbols.symbols_with_pos_enabled_symbol,
            symbols_with_pos_enabled,
            print_symbols_bare_symbol: core_eval_symbols.print_symbols_bare_symbol,
            print_symbols_bare,
            features,
            require_stack,
            loads_in_progress,
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
            treesit: super::treesit::TreeSitterManager::new(),
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
            quit_requested: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
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
            gc_runtime_settings_cache: GcRuntimeSettingsCache::default(),
            vm_root_frames: Vec::new(),
            bc_buf: Vec::with_capacity(4096),
            bc_frames: Vec::new(),
            condition_stack: Vec::new(),
            next_resume_id: 1,
            pending_safe_funcalls: Vec::new(),
            named_call_cache: HashMap::with_capacity(NAMED_CALL_CACHE_CAPACITY),
            lexenv_assq_cache: RefCell::new(LexenvAssqCache::default()),
            lexenv_special_cache: RefCell::new(LexenvSpecialCache::default()),

            macro_expansion_scope_depth: 0,
            macro_expansion_mutation_epoch: 0,
            macro_cache_hits: 0,
            macro_cache_misses: 0,
            macro_expand_total_us: 0,
            macro_cache_disabled: false,
            runtime_macro_expansion_cache: HashMap::new(),
            macro_perf_enabled: std::env::var_os("NEOVM_TRACE_MACRO_PERF").is_some(),
            macro_perf_stats: MacroPerfStats::default(),
            interpreted_closure_filter_fn: None,
            interpreted_closure_trim_cache: HashMap::new(),
            interpreted_closure_value_cache: HashMap::new(),
        };
        ev.initialize_gc_stack_bottom();
        ev.setup_thread_locals();

        // Rebuild the builtin subr registry after pdump restore. The dumped
        // obarray already carries the authoritative runtime function-cell
        // surface, so restore that surface immediately afterward.
        builtins::init_builtins(&mut ev);
        for (sym_id, symbol) in dumped_function_surface.iter_symbols() {
            if !symbol.function.is_nil() {
                ev.obarray.set_symbol_function_id(sym_id, symbol.function);
            } else if dumped_function_surface.is_function_unbound_id(sym_id) {
                ev.obarray.fmakunbound_id(sym_id);
            } else {
                ev.obarray.clear_function_silent_id(sym_id);
            }
        }

        ev.finish_runtime_activation(true);

        ev
    }

    // -----------------------------------------------------------------------
    // Garbage collection
    // -----------------------------------------------------------------------

    /// Enumerate every live `Value` reference in the evaluator and all
    /// sub-managers without materializing a single temporary root vector.
    fn trace_roots(&self, visit: &mut dyn FnMut(Value)) {
        for frame in &self.vm_root_frames {
            for root in frame.roots.iter().copied() {
                visit(root);
            }
        }
        for root in self.treesit.roots() {
            visit(root);
        }
        for root in self.bc_buf.iter().copied() {
            visit(root);
        }
        for frame in &self.bc_frames {
            if frame.fun.is_heap_object() {
                visit(frame.fun);
            }
        }
        for frame in &self.condition_stack {
            match frame {
                ConditionFrame::Catch { tag, .. } => visit(*tag),
                ConditionFrame::ConditionCase { conditions, .. } => visit(*conditions),
                ConditionFrame::HandlerBind {
                    conditions,
                    handler,
                    ..
                } => {
                    visit(*conditions);
                    visit(*handler);
                }
                ConditionFrame::SkipConditions { .. } => {}
            }
        }
        for entry in &self.specpdl {
            match entry {
                SpecBinding::Let {
                    old_value: Some(val),
                    ..
                } => visit(*val),
                SpecBinding::LetLocal { old_value, .. } => visit(*old_value),
                SpecBinding::LetDefault {
                    old_value: Some(val),
                    ..
                } => visit(*val),
                SpecBinding::LexicalEnv { old_lexenv } => visit(*old_lexenv),
                SpecBinding::GcRoot { value } => visit(*value),
                SpecBinding::Backtrace { function, args, .. } => {
                    visit(*function);
                    for arg in args.iter().copied() {
                        visit(arg);
                    }
                }
                SpecBinding::UnwindProtect { forms, lexenv } => {
                    visit(*forms);
                    visit(*lexenv);
                }
                SpecBinding::SaveRestriction { state } => {
                    let mut roots = Vec::new();
                    state.trace_roots(&mut roots);
                    for root in roots {
                        visit(root);
                    }
                }
                SpecBinding::SaveExcursion { .. }
                | SpecBinding::SaveCurrentBuffer { .. }
                | SpecBinding::Nop => {}
                _ => {}
            }
        }
        visit(self.lexenv);
        for entry in self.runtime_macro_expansion_cache.values() {
            visit(entry.expanded);
        }
        for bucket in self.interpreted_closure_trim_cache.values() {
            for entry in bucket {
                visit(entry.params_value);
                visit(entry.body_value);
                visit(entry.iform_value);
                visit(entry.trimmed_params_value);
                visit(entry.trimmed_body_value);
            }
        }
        for bucket in self.interpreted_closure_value_cache.values() {
            for entry in bucket {
                visit(entry.source_function);
                visit(entry.trimmed_params_value);
                visit(entry.trimmed_body_value);
            }
        }
        if let Some(filter_fn) = self.interpreted_closure_filter_fn {
            visit(filter_fn);
        }
        for entry in self.named_call_cache.values() {
            if let NamedCallTarget::Obarray(val) = &entry.target {
                visit(*val);
            }
        }
        for funcall in &self.pending_safe_funcalls {
            visit(funcall.function);
            for arg in funcall.args.iter().copied() {
                visit(arg);
            }
        }
        let mut thread_local_roots = Vec::new();
        collect_thread_local_gc_roots(&mut thread_local_roots);
        for root in thread_local_roots {
            visit(root);
        }
        if !self.current_local_map.is_nil() {
            visit(self.current_local_map);
        }
        if self.standard_syntax_table.is_heap_object() {
            visit(self.standard_syntax_table);
        }
        if self.standard_category_table.is_heap_object() {
            visit(self.standard_category_table);
        }
        self.obarray.trace_roots_with(visit);
        self.processes.trace_roots_with(visit);
        self.timers.trace_roots_with(visit);
        self.watchers.trace_roots_with(visit);
        self.registers.trace_roots_with(visit);
        self.custom.trace_roots_with(visit);
        self.autoloads.trace_roots_with(visit);
        self.buffers.trace_roots_with(visit);
        self.face_table.trace_roots_with(visit);
        self.threads.trace_roots_with(visit);
        self.kmacro.trace_roots_with(visit);
        crate::gc_trace::GcTrace::trace_roots_with(&self.command_loop, visit);
        self.modes.trace_roots_with(visit);
        self.frames.trace_roots_with(visit);
        self.coding_systems.trace_roots_with(visit);
        if let Some(ref md) = self.match_data
            && let Some(crate::emacs_core::regex::SearchedString::Heap(val)) = &md.searched_string
        {
            visit(*val);
        }
    }

    /// Get the current GC threshold.
    pub fn gc_threshold(&self) -> usize {
        self.tagged_heap.gc_threshold()
    }

    fn is_gc_runtime_setting_symbol(sym_id: SymId) -> bool {
        sym_id == gc_cons_threshold_symbol()
            || sym_id == gc_cons_percentage_symbol()
            || sym_id == memory_full_symbol()
    }

    pub(crate) fn refresh_gc_runtime_settings_after_change_by_id(&mut self, sym_id: SymId) {
        if Self::is_gc_runtime_setting_symbol(sym_id) {
            self.refresh_gc_runtime_settings_cache();
            self.sync_gc_threshold_from_runtime_settings();
        }
    }

    fn refresh_gc_runtime_settings_cache(&mut self) {
        self.gc_runtime_settings_cache.gc_cons_threshold_bytes = self
            .obarray
            .symbol_value_id(gc_cons_threshold_symbol())
            .copied()
            .and_then(|value| value.as_fixnum())
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(GC_DEFAULT_THRESHOLD_BYTES);
        self.gc_runtime_settings_cache.gc_cons_percentage_scaled = self
            .obarray
            .symbol_value_id(gc_cons_percentage_symbol())
            .copied()
            .unwrap_or(Value::NIL)
            .as_number_f64()
            .filter(|float| float.is_finite() && *float > 0.0)
            .map(|float| ((float * GC_PERCENT_SCALE as f64).ceil() as u64).clamp(1, u64::MAX));
        self.gc_runtime_settings_cache.memory_full = !self
            .obarray
            .symbol_value_id(memory_full_symbol())
            .copied()
            .unwrap_or(Value::NIL)
            .is_nil();
    }

    fn effective_gc_threshold_bytes(&mut self) -> usize {
        if self.gc_runtime_settings_cache.memory_full {
            return self.tagged_heap.gc_threshold();
        }

        let mut threshold = self
            .gc_runtime_settings_cache
            .gc_cons_threshold_bytes
            .max(GC_THRESHOLD_FLOOR_BYTES);
        if let Some(percentage_scaled) = self.gc_runtime_settings_cache.gc_cons_percentage_scaled {
            let live_estimate = self
                .tagged_heap
                .live_bytes()
                .saturating_add(self.tagged_heap.bytes_since_gc() / 2);
            let pct_threshold = ((live_estimate as u128)
                .saturating_mul(percentage_scaled as u128)
                .saturating_add((GC_PERCENT_SCALE - 1) as u128)
                / GC_PERCENT_SCALE as u128)
                .min(GC_HI_THRESHOLD_BYTES as u128) as usize;
            threshold = threshold.max(pct_threshold);
        }
        threshold.clamp(1, GC_HI_THRESHOLD_BYTES)
    }

    fn sync_gc_threshold_from_runtime_settings(&mut self) {
        let threshold = self.effective_gc_threshold_bytes();
        if self.tagged_heap.gc_threshold() != threshold {
            self.tagged_heap.set_gc_threshold_from_runtime(threshold);
        }
    }

    fn update_gc_runtime_stats(&mut self, elapsed: std::time::Duration) {
        self.obarray
            .set_symbol_value_id(gcs_done_symbol(), Value::fixnum(self.gc_count as i64));

        let old_elapsed = self
            .obarray
            .symbol_value_id(gc_elapsed_symbol())
            .copied()
            .and_then(|value| value.as_number_f64())
            .unwrap_or(0.0);
        self.obarray.set_symbol_value_id(
            gc_elapsed_symbol(),
            Value::make_float(old_elapsed + elapsed.as_secs_f64()),
        );
    }

    /// Set the GC threshold. Use usize::MAX to effectively disable GC.
    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.tagged_heap.set_gc_threshold(threshold);
    }

    /// Set the maximum eval recursion depth.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// Set the thread-local heap pointers for the current thread.
    ///
    /// Must be called when using an Context from a thread other than the one
    /// that created it (e.g., in worker thread pools).
    pub fn setup_thread_locals(&mut self) {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        super::syntax::restore_standard_syntax_table_object(self.standard_syntax_table);
        super::category::restore_standard_category_table_object(self.standard_category_table);
        // Install this Context's quit-request flag so leaf functions
        // (regex matcher, other long-running scans) can poll it
        // without `&mut Context` access.
        QUIT_REQUESTED_TLS.with(|cell| {
            *cell.borrow_mut() = Some(std::sync::Arc::clone(&self.quit_requested));
        });
    }

    fn initialize_gc_stack_bottom(&mut self) {
        #[cfg(target_os = "linux")]
        {
            if let Some(stack_end) = crate::tagged::gc::read_stack_end_from_proc() {
                self.tagged_heap.set_stack_bottom(stack_end as *const u8);
            }
        }
    }

    fn finish_runtime_activation(&mut self, sync_keyboard: bool) {
        self.setup_thread_locals();
        self.refresh_gc_runtime_settings_cache();
        self.sync_gc_threshold_from_runtime_settings();
        if sync_keyboard {
            self.sync_keyboard_runtime_from_obarray();
        }
        self.sync_thread_runtime_bindings();
        self.sync_current_thread_buffer_state();
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
            Err(Flow::Signal(sig)) if sig.symbol == self.kill_emacs_symbol => Ok(()),
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
        // Interactive command loops need an input source. Batch mode is
        // different: GNU still runs `top-level`/`normal-top-level` and lets
        // `read_char` terminate the loop via noninteractive EOF, even when
        // there is no input channel at all.
        if self.input_rx.is_none() && !self.command_loop_noninteractive() {
            tracing::info!("recursive_edit_inner: no input receiver, returning immediately");
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

            // GNU keyboard.c command_loop():
            //   internal_catch (Qtop_level, top_level_1, Qnil);
            //   internal_catch (Qtop_level, command_loop_2, Qerror);
            // Both top_level_1 and command_loop_2 run unconditionally per
            // outer loop iteration. The catch around top_level_1 turns any
            // 'top-level throw into a normal return so the next line — the
            // command_loop_2 catch — still runs. The previous NeoMacs
            // implementation gated command_loop_2 on
            // `self.command_loop.running`, which incorrectly skipped the
            // interactive loop entirely whenever (normal-top-level) raised
            // an error caught inside command_loop_top_level_1: the GUI
            // would create its window, hit the error, return Ok(NIL), and
            // immediately exit before the first redisplay. Match GNU and
            // always run command_loop_2 after top_level_1.
            let result = if outermost_command_loop {
                match self.command_loop_top_level_1() {
                    Ok(_) => self.command_loop_2(),
                    Err(Flow::Throw { ref tag, .. }) if tag.is_symbol_named("top-level") => {
                        // top-level throw inside top_level_1 — fall through
                        // to command_loop_2 just like GNU's two-catch flow.
                        self.command_loop_2()
                    }
                    Err(flow) => Err(flow),
                }
            } else {
                self.command_loop_2()
            };

            self.pop_condition_frame();

            match result {
                // top-level throw → restart the loop
                Err(Flow::Throw { ref tag, .. }) if tag.is_symbol_named("top-level") => {
                    tracing::debug!("command_loop_inner: top-level throw, restarting loop");
                    continue;
                }
                Ok(value) if outermost_command_loop && self.command_loop_noninteractive() => {
                    // GNU keyboard.c:1145 — end of file in batch run
                    tracing::info!("command_loop_inner: noninteractive EOF, calling kill-emacs");
                    super::builtins::symbols::builtin_kill_emacs(self, vec![Value::T])?;
                    return Ok(value);
                }
                // Any other result propagates up
                other => {
                    tracing::debug!(
                        "command_loop_inner: result={:?}, propagating",
                        other.is_ok()
                    );
                    return other;
                }
            }
        }
    }

    fn command_loop_noninteractive(&self) -> bool {
        self.noninteractive
    }

    fn command_loop_top_level_1(&mut self) -> EvalResult {
        let top_level = self
            .obarray
            .symbol_value("top-level")
            .copied()
            .unwrap_or(Value::NIL);

        tracing::debug!("command_loop_top_level_1: top-level={}", top_level);

        if top_level.is_nil() {
            tracing::debug!("command_loop_top_level_1: top-level is nil, skipping");
            self.log_startup_state("top-level-nil");
            return Ok(Value::NIL);
        }

        tracing::debug!("command_loop_top_level_1: evaluating top-level form");
        self.log_startup_state("top-level-before");
        match self.eval_value(&top_level) {
            Ok(_) => {
                tracing::debug!("command_loop_top_level_1: top-level completed OK");
                self.log_startup_state("top-level-after");
                Ok(Value::NIL)
            }
            Err(Flow::Signal(sig)) => {
                tracing::warn!(
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
            .map(|buffer| buffer.name_runtime_string_owned())
            .unwrap_or_else(|| "<none>".to_string());
        let selected_frame = self.frames.selected_frame().map(|frame| {
            let selected_window_buffer = frame
                .selected_window()
                .and_then(|window| window.buffer_id())
                .and_then(|buffer_id| self.buffers.get(buffer_id))
                .map(|buffer| buffer.name_runtime_string_owned())
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
            self.sync_current_buffer_to_selected_window();

            if self.executing_kbd_macro_iteration_complete_for_command_loop() {
                self.assign("this-command", Value::NIL);
                return Ok(Value::NIL);
            }

            // Transfer prefix-arg → current-prefix-arg, saving the
            // outgoing current-prefix-arg into last-prefix-arg
            // first. Mirrors GNU `keyboard.c:1329`:
            //
            //   Vlast_prefix_arg = Vcurrent_prefix_arg;
            //
            // Keyboard audit Finding 15.
            let outgoing_prefix_arg = self.eval_symbol("current-prefix-arg").unwrap_or(Value::NIL);
            self.assign("last-prefix-arg", outgoing_prefix_arg);
            let prefix_arg = self.eval_symbol("prefix-arg").unwrap_or(Value::NIL);
            self.assign("current-prefix-arg", prefix_arg);
            self.assign("prefix-arg", Value::NIL);

            // Read a complete key sequence (may be multi-key, e.g. C-x C-f).
            let (keys, binding) = self.read_key_sequence()?;
            self.sync_current_buffer_to_selected_window();

            if keys.is_empty() && binding.is_nil() {
                self.assign("this-command", Value::NIL);
                return Ok(Value::NIL);
            }

            if binding.is_nil() {
                // Undefined key sequence — reset prefix arg
                self.assign("prefix-arg", Value::NIL);
                let desc: Vec<String> = keys.iter().map(|v| format!("{:?}", v)).collect();
                tracing::info!("Undefined key sequence: {}", desc.join(" "));
                continue;
            }

            // Set this-command, real-this-command, last-command-event,
            // this-command-keys. Mirrors GNU `keyboard.c:1336-1353`:
            //
            //   Vreal_last_command = KVAR (current_kboard, Vreal_last_command);
            //   Vreal_this_command = cmd;
            //   /* Now do the remap. */
            //   cmd = Fcommand_remapping (cmd, Qnil, Qnil);
            //   ...
            //   Vthis_command = cmd;
            //   if (NILP (Vthis_original_command))
            //     Vthis_original_command = cmd;
            //
            // Keyboard audit Findings 1, 2, 3, 4 in
            // `drafts/keyboard-command-loop-audit.md`.
            tracing::info!("command_loop_1: dispatching binding={}", binding);

            // Snapshot the previous this-command into real-last-command
            // before we overwrite (Finding 3 — GNU
            // `keyboard.c:1336-1339`).
            let previous_this_command = self.eval_symbol("this-command").unwrap_or(Value::NIL);
            self.assign("real-last-command", previous_this_command);

            // The unmapped command (real-this-command) is the binding
            // we read from the keymap, before any remapping is applied.
            self.assign("real-this-command", binding);

            // Apply command remapping per GNU
            // `keyboard.c:1340-1343`. The remapped command becomes
            // this-command for execution. Finding 4.
            let remapped = self.command_remapping_for_loop(binding);
            self.assign("this-command", remapped);

            // Finding 2: this-original-command stays at the original
            // (pre-remap) command for the duration of the iteration
            // unless a pre-command-hook explicitly cleared it.
            if self
                .eval_symbol("this-original-command")
                .unwrap_or(Value::NIL)
                .is_nil()
            {
                self.assign("this-original-command", binding);
            }

            if let Some(last) = keys.last() {
                self.assign("last-command-event", *last);
            }
            tracing::debug!(
                "command_loop_1: binding={} current_buffer={:?} active_minibuffer_window={:?}",
                self.this_command_name_for_log(),
                self.buffers.current_buffer_id(),
                self.active_minibuffer_window
            );

            // Run pre-command-hook via safe-run-hooks so a broken
            // hook function is removed instead of re-firing on every
            // command. Finding 7 — GNU `keyboard.c:1361`
            // (`safe_run_hooks (Qpre_command_hook)`).
            self.safe_run_hook_if_bound("pre-command-hook");

            // Execute the command. Finding 13: GNU calls
            // `Fcall_interactively` directly from the loop. We do
            // the same by routing through the call-interactively
            // builtin instead of the Lisp-side command-execute
            // wrapper.
            let exec_result = self.dispatch_command_in_loop(binding);

            // Keep the selected window's point and current buffer/runtime view
            // aligned before post-command work and redisplay observe state.
            self.sync_current_buffer_to_selected_window();

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

            // Update last-command (GNU `keyboard.c:1473`).
            if let Ok(this_cmd) = self.eval_symbol("this-command") {
                self.assign("last-command", this_cmd);
            }

            // Update last-repeatable-command per GNU
            // `keyboard.c:1467-1470`. Finding 3.
            //
            //   KVAR (current_kboard, Vlast_repeatable_command)
            //     = (EQ (Vreal_this_command, Qself_insert_command)
            //        ? Vreal_last_command
            //        : Vreal_this_command);
            let real_this = self.eval_symbol("real-this-command").unwrap_or(Value::NIL);
            let is_self_insert = real_this
                .as_symbol_name()
                .is_some_and(|n| n == "self-insert-command");
            let last_repeatable = if is_self_insert {
                self.eval_symbol("real-last-command").unwrap_or(Value::NIL)
            } else {
                real_this
            };
            self.assign("last-repeatable-command", last_repeatable);

            // GNU runs deactivate-mark handling BEFORE
            // post-command-hook (`keyboard.c:1471-1484`), so the
            // hook sees the post-deactivate state. Finding 14.
            let _ = self.update_active_region_selection_after_command();

            // Run post-command-hook via safe-run-hooks (Finding 7).
            self.safe_run_hook_if_bound("post-command-hook");

            // Reset this-original-command for the next iteration so
            // a fresh command starts the cycle clean (mirroring
            // GNU's clear at the bottom of command_loop_1).
            self.assign("this-original-command", Value::NIL);

            if exec_result.is_ok()
                && self.command_loop.keyboard.kboard.defining_kbd_macro
                && self
                    .eval_symbol("prefix-arg")
                    .unwrap_or(Value::NIL)
                    .is_nil()
            {
                self.finalize_kbd_macro_runtime_chars();
            }

            // Keyboard audit Finding 9: auto-save-interval check.
            // GNU `keyboard.c:1491-1506`:
            //
            //   if (INTEGERP (Vauto_save_interval)
            //       && num_nonmacro_input_events - last_auto_save
            //          > max (XFIXNUM (Vauto_save_interval), 20)
            //       && !detect_input_pending_run_timers (0))
            //     {
            //       Fdo_auto_save (Qnil, Qnil);
            //       last_auto_save = num_nonmacro_input_events;
            //       ...
            //     }
            //
            // The lower floor of 20 prevents saving too often if
            // a user sets `auto-save-interval` to a tiny value.
            // The `detect_input_pending` gate defers the save
            // when the user is typing faster than the check
            // interval — we approximate that with a "no pending
            // events in the unread queue" probe.
            self.command_loop_1_maybe_auto_save();
        }
    }

    /// Per-iteration `auto-save-interval` check, mirroring GNU
    /// `keyboard.c:1491-1506`. Keyboard audit Finding 9.
    fn command_loop_1_maybe_auto_save(&mut self) {
        let interval = match self.eval_symbol("auto-save-interval").ok() {
            Some(v) => match v.as_fixnum() {
                Some(n) if n > 0 => n as u64,
                _ => return,
            },
            None => return,
        };
        let threshold = interval.max(20);
        let current = self.command_loop.num_nonmacro_input_events;
        let last = self.command_loop.last_auto_save_input_events;
        if current.saturating_sub(last) <= threshold {
            return;
        }
        // Defer if input is pending (same spirit as GNU's
        // `detect_input_pending_run_timers (0)` gate). A fast
        // typist should not be interrupted by a save.
        if self.input_pending_for_auto_save() {
            return;
        }
        // Record the attempt *before* calling do-auto-save so a
        // throw/signal from the save doesn't loop the check on
        // every iteration. GNU writes `last_auto_save` after the
        // call but also swallows errors via cmd_error; setting
        // it first is safer in our flow-based error model.
        self.command_loop.last_auto_save_input_events = current;
        if let Err(flow) = self.apply(Value::symbol("do-auto-save"), vec![Value::NIL, Value::NIL]) {
            tracing::warn!("auto-save from command_loop_1 failed: {:?}", flow);
        }
    }

    /// Approximation of GNU `detect_input_pending_run_timers (0)`
    /// used by the command-loop auto-save gate. Returns true when
    /// there is already-queued input that should run before an
    /// expensive auto-save.
    fn input_pending_for_auto_save(&self) -> bool {
        if self.peek_unread_command_event().is_some() {
            return true;
        }
        !self.command_loop.keyboard.pending_input_events.is_empty()
    }

    /// Apply `command-remapping` for the command-loop dispatch
    /// path. Mirrors GNU `keyboard.c:1340-1343` calling
    /// `Fcommand_remapping (cmd, Qnil, Qnil)` and substituting the
    /// result when non-nil. Keyboard audit Finding 4.
    fn command_remapping_for_loop(&mut self, command: Value) -> Value {
        if command.is_nil() {
            return command;
        }
        match self.apply(Value::symbol("command-remapping"), vec![command]) {
            Ok(remapped) if !remapped.is_nil() => remapped,
            _ => command,
        }
    }

    /// Dispatch the current `this-command` via `call-interactively`,
    /// matching GNU's direct `Fcall_interactively (cmd, ...)` call
    /// at `keyboard.c:1449-1457`.
    ///
    /// Routing through the Rust builtin (instead of the Lisp
    /// `command-execute` wrapper) removes a layer of indirection
    /// and prevents user-side advice on `command-execute` from
    /// silently changing command-loop behavior. Keyboard audit
    /// Finding 13.
    fn dispatch_command_in_loop(&mut self, command: Value) -> EvalResult {
        // Re-resolve `this-command` from the obarray so a
        // pre-command-hook that mutated the symbol takes effect.
        let cmd = self.eval_symbol("this-command").unwrap_or(command);
        if cmd.is_nil() {
            return Ok(Value::NIL);
        }
        // GNU `Fcall_interactively (cmd, Qnil, Qnil)` —
        // `record-flag = nil`, `keys = nil`. The first arg is the
        // command, the optional record-flag selects whether to
        // record into command-history.
        self.apply(Value::symbol("call-interactively"), vec![cmd])
    }

    /// Run a hook with `safe-run-hooks` semantics: each hook
    /// function is wrapped in a `condition-case` so a broken
    /// function is removed from the hook instead of re-firing on
    /// every subsequent command. Mirrors GNU
    /// `safe_run_hooks (Qhook_name)` at
    /// `src/keyboard.c:1361,1485` and `src/eval.c:2779-2830`.
    /// Keyboard audit Finding 7.
    fn safe_run_hook_if_bound(&mut self, hook_name: &str) {
        // GNU `keyboard.c:1970-1978` (`safe_run_hooks`):
        //
        //   void safe_run_hooks (Lisp_Object hook) {
        //     specbind (Qinhibit_quit, Qt);
        //     run_hook_with_args (2, {hook, hook}, safe_run_hook_funcall);
        //     unbind_to (count, Qnil);
        //   }
        //
        // This is a C function — NOT the Lisp `safe-run-hooks` from
        // `subr.el`. It calls `run_hook_with_args` with a custom
        // funcall wrapper (`safe_run_hook_funcall`) that wraps each
        // hook function in `internal_condition_case_n` and removes
        // broken entries on error.
        //
        // neomacs mirrors this by calling
        // `hook_runtime::safe_run_named_hook` directly from Rust,
        // which resolves the hook value (including buffer-local
        // bindings + the `t` global marker), calls each hook
        // function, and swallows Signal errors. This never goes
        // through Lisp — matching GNU's keyboard.c which calls the
        // C function, not the Lisp wrapper.
        let hook_sym = super::intern::intern(hook_name);
        let _ = super::hook_runtime::safe_run_named_hook(self, hook_sym, &[]);
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
        self.pending_safe_funcalls.push(PendingSafeFuncall {
            function,
            args: args.into_iter().collect(),
        });
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
            let _ = self.apply(funcall.function, funcall.args.into_iter().collect());
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
        // Mirrors GNU `redisplay_internal` (xdisp.c:17242-17245): bail out
        // when `inhibit-redisplay` is non-nil. `run_window_change_functions`
        // (window.c:4116) specbinds this to t so any nested redisplay
        // triggered by a window-change hook is a no-op. Without this check
        // a hook that indirectly calls `redisplay` infinitely recurses.
        if self
            .obarray
            .symbol_value("inhibit-redisplay")
            .is_some_and(|v| v.is_truthy())
        {
            return;
        }
        self.sync_pending_resize_events();
        // GNU Emacs xdisp.c:20616 — sync selected window's pointm from
        // the buffer's current PT before redisplay.  NeoMacs Window::point
        // is a plain usize, not a marker, so it doesn't auto-update.
        // Only sync when the buffer has been modified (to avoid breaking
        // the initial render where window.point=1 is correct).
        if let Some(buffer) = self.buffers.current_buffer() {
            if buffer.is_modified() {
                let pt = buffer.point_char();
                if let Some(frame) = self.frames.selected_frame_mut() {
                    if let Some(win) = frame.selected_window_mut() {
                        win.set_point(pt);
                    }
                }
            }
        }
        let has_fn = self.redisplay_fn.is_some();
        tracing::debug!("redisplay called (has_fn={})", has_fn);
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
        self.gc_collect_exact();
    }

    /// Perform a full mark-and-sweep garbage collection using only explicit roots.
    #[tracing::instrument(level = "debug", skip(self))]
    pub fn gc_collect_exact(&mut self) {
        self.gc_collect_from_current_roots();
    }

    fn gc_collect_from_current_roots(&mut self) {
        let start = std::time::Instant::now();
        *self.lexenv_assq_cache.borrow_mut() = LexenvAssqCache::default();
        *self.lexenv_special_cache.borrow_mut() = LexenvSpecialCache::default();
        let heap_ptr: *mut crate::tagged::gc::TaggedHeap = &mut *self.tagged_heap;
        // Safety: GC is stop-the-world with exclusive `&mut self`. Root
        // enumeration only reads Context state while seeding the collector via
        // the raw heap pointer.
        unsafe {
            (*heap_ptr).begin_collection();
            self.trace_roots(&mut |root| {
                (*heap_ptr).seed_root(root);
            });
            // Install per-buffer marker-chain head slots so
            // `unchain_dead_markers` can splice unmarked markers out of
            // every live chain before sweep. Mirrors GNU
            // `sweep_buffer → unchain_dead_markers` (alloc.c).
            let chain_heads = self.buffers.collect_marker_chain_head_slots();
            (*heap_ptr).set_marker_chain_head_slots(chain_heads);
            (*heap_ptr).complete_collection();
        }
        self.gc_pending = false;
        self.gc_count += 1;
        self.update_gc_runtime_stats(start.elapsed());
        self.sync_gc_threshold_from_runtime_settings();
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

    /// GC safe point used at evaluator boundaries.
    pub fn gc_safe_point(&mut self) {
        self.gc_safe_point_exact();
    }

    /// Trigger a safe-point collection using only explicit evaluator roots.
    pub(crate) fn gc_safe_point_exact(&mut self) {
        if self.gc_inhibit_depth > 0 {
            return;
        }
        if self.gc_stress || self.gc_pending {
            self.gc_collect_from_current_roots();
            return;
        }

        if self.tagged_heap.gc_threshold_is_overridden() {
            if self.tagged_heap.should_collect() {
                self.gc_collect_from_current_roots();
            }
            return;
        }

        let threshold = self.effective_gc_threshold_bytes();
        if self.tagged_heap.bytes_since_gc() >= threshold {
            if self.tagged_heap.gc_threshold() != threshold {
                self.tagged_heap.set_gc_threshold_from_runtime(threshold);
            }
            self.gc_collect_from_current_roots();
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
    pub(crate) fn maybe_quit(&mut self) -> Result<(), Flow> {
        // Drain the cross-thread quit-request atomic into `Vquit_flag`.
        // Set by the input-bridge thread when it observes a `quit-char`
        // keystroke while the evaluator is busy (e.g. deep in bytecode
        // and not reading from `input_rx`). See
        // `Context::quit_requested` for the design rationale.
        if self
            .quit_requested
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            if self
                .obarray
                .symbol_value_id(self.quit_flag_symbol)
                .copied()
                .unwrap_or(Value::NIL)
                .is_nil()
            {
                self.obarray
                    .set_symbol_value_id(self.quit_flag_symbol, Value::T);
            }
        }
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
        // GNU's keyboard/input path treats batch mode as a separate path and
        // does not poll window-system input while `noninteractive` is set.
        // Neomacs should likewise skip resize/input syncing during batch
        // bootstrap and batch command loops.
        if self.command_loop_noninteractive() {
            return;
        }

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
    ///
    /// The remaining evaluator entry points either root their live Values
    /// explicitly or run before materializing heap-backed Values, so this path
    /// now uses exact roots rather than conservative stack scanning.
    fn maybe_gc_and_quit(&mut self) -> Result<(), Flow> {
        self.poll_pending_input_for_throw_on_input();
        self.maybe_quit()?;
        self.gc_safe_point_exact();
        Ok(())
    }

}

impl Context {
    #[inline]
    fn maybe_grow_eval_stack<R>(&mut self, callback: impl FnOnce(&mut Self) -> R) -> R {
        let depth = self.depth;
        if depth < STACK_GROWTH_PROBE_START_DEPTH
            || !depth.is_multiple_of(STACK_GROWTH_PROBE_INTERVAL)
        {
            return callback(self);
        }
        stacker::maybe_grow(EVAL_STACK_RED_ZONE, EVAL_STACK_SEGMENT, || callback(self))
    }

    /// Whether lexical-binding is currently enabled.
    pub fn lexical_binding(&self) -> bool {
        lexenv_is_active(self.lexenv)
    }

    pub(crate) fn current_input_mode_tuple(&self) -> (bool, bool, bool, i64) {
        // Batch oracle compatibility: flow-control and meta are fixed to
        // nil/t respectively; quit-char remains mutable like GNU Emacs.
        (self.input_mode_interrupt, false, true, self.quit_char)
    }

    pub(crate) fn set_input_mode_interrupt(&mut self, interrupt: bool) {
        self.input_mode_interrupt = interrupt;
    }

    #[inline]
    fn sync_cached_runtime_binding_by_id(&mut self, sym_id: SymId, value: Value) {
        if sym_id == self.noninteractive_symbol {
            self.noninteractive = value.is_truthy();
        } else if sym_id == self.symbols_with_pos_enabled_symbol {
            self.symbols_with_pos_enabled = value.is_truthy();
        } else if sym_id == self.print_symbols_bare_symbol {
            self.print_symbols_bare = value.is_truthy();
        }
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

    /// Set the file-level `lexical-binding` (per-buffer) and sync the
    /// top-level lexical environment.
    ///
    /// Called at file-loading boundaries (load.rs, lread.rs) and test
    /// setup. Mirrors GNU Emacs where the file loader both sets the
    /// `lexical-binding` buffer-local AND specbinds
    /// `Vinternal_interpreter_environment` to `(t)` or `nil`.
    ///
    /// Uses `set_variable` which routes through the buffer-local
    /// FORWARDED dispatch (matching GNU where `lexical-binding` is
    /// `DEFVAR_PER_BUFFER` in buffer.c). Each buffer gets its own
    /// `lexical-binding` value from its file's -*- cookie.
    ///
    /// Note: `Feval` (begin_eval_with_lexical_arg) does NOT call this.
    /// `Feval` only saves/restores `self.lexenv` without touching the
    /// per-buffer `lexical-binding`, matching GNU where nested eval
    /// calls never clobber the file-level setting.
    pub fn set_lexical_binding(&mut self, enabled: bool) {
        self.set_variable("lexical-binding", Value::bool_val(enabled));
        if enabled {
            if self.lexenv.is_nil() {
                self.lexenv = top_level_lexenv_sentinel();
            }
        } else if is_top_level_lexenv_sentinel(self.lexenv) {
            self.lexenv = Value::NIL;
        }
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
        self.lexenv = if lexical_binding_in_obarray(&self.obarray) {
            top_level_lexenv_sentinel()
        } else {
            Value::NIL
        };
        self.condition_stack.clear();
        self.depth = 0;
        // Named-call resolution is a runtime memoization layer, not part of
        // GNU's persisted Lisp surface. If it survives bootstrap/pdump
        // boundaries it can disagree with restored function cells while still
        // carrying a matching function epoch.
        self.named_call_cache.clear();
    }

    #[cfg(test)]
    pub(crate) fn top_level_eval_state_is_clean(&self) -> bool {
        let clean_lexenv = self.lexenv.is_nil()
            || (self.lexical_binding() && is_top_level_lexenv_sentinel(self.lexenv));
        self.specpdl.is_empty()
            && clean_lexenv
            && self.vm_root_frames.is_empty()
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
            self.interpreted_closure_value_cache.clear();
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
            &mut self.specpdl,
            lexical_arg,
        )?;
        let result = self.eval_value(&form);
        finish_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.specpdl,
            state,
        );
        result
    }

    pub(crate) fn eval_lambda_body_value(&mut self, body: Value) -> EvalResult {
        self.maybe_grow_eval_stack(|ctx| {
            let mut cursor = body;
            let mut last = Value::NIL;
            while cursor.is_cons() {
                last = ctx.eval_sub(cursor.cons_car())?;
                cursor = cursor.cons_cdr();
            }
            Ok(last)
        })
    }

    pub(crate) fn begin_lambda_call(
        &mut self,
        params: &LambdaParams,
        env: Option<Value>,
        args: &[Value],
    ) -> Result<ActiveLambdaCallState, Flow> {
        begin_lambda_call_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.lexenv,
            params,
            env,
            args,
        )
    }

    pub(crate) fn finish_lambda_call(&mut self, state: ActiveLambdaCallState) {
        finish_lambda_call_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.lexenv,
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

    pub fn current_message_text(&self) -> Option<String> {
        self.current_message
            .as_ref()
            .map(|message| crate::emacs_core::builtins::runtime_string_from_lisp_string(message))
    }

    pub fn current_message_value(&self) -> Option<Value> {
        self.current_message
            .as_ref()
            .map(|message| Value::heap_string(message.clone()))
    }

    pub fn set_current_message(&mut self, message: Option<crate::heap_types::LispString>) {
        self.current_message = message;
    }

    pub(crate) fn append_current_message_runtime_text(&mut self, text: &str) {
        let multibyte = self
            .current_message
            .as_ref()
            .map(crate::heap_types::LispString::is_multibyte)
            .unwrap_or(true);
        let piece = crate::emacs_core::builtins::runtime_string_to_lisp_string(text, multibyte);
        self.append_current_message_lisp_string(&piece);
    }

    pub(crate) fn append_current_message_lisp_string(
        &mut self,
        text: &crate::heap_types::LispString,
    ) {
        match self.current_message.as_mut() {
            Some(message) => *message = message.concat(text),
            None => self.current_message = Some(text.clone()),
        }
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

    pub(crate) fn current_message_slot(&mut self) -> &mut Option<crate::heap_types::LispString> {
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

    /// Evaluate a Lisp expression string. Convenience for tests.
    /// Reads via the Value-native reader and evaluates via eval_sub.
    pub fn eval_str(&mut self, source: &str) -> Result<Value, EvalError> {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        let forms = super::value_reader::read_all(source).map_err(|e| EvalError::Signal {
            symbol: crate::emacs_core::intern::intern("error"),
            data: vec![Value::string(format!("Read error: {}", e.message))],
            raw_data: None,
        })?;
        if forms.is_empty() {
            return Ok(Value::NIL);
        }
        // Root every parsed form: each `eval_sub` call may trigger GC, and
        // the un-iterated forms still sitting in the heap-allocated Vec are
        // otherwise invisible to the exact root walk.
        let specpdl_root_scope = self.save_specpdl_roots();
        for form in &forms {
            self.push_specpdl_root(*form);
        }
        let mut result = Value::NIL;
        let mut error = None;
        for form in &forms {
            match self.eval_sub(*form).map_err(super::error::map_flow) {
                Ok(v) => result = v,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        self.restore_specpdl_roots(specpdl_root_scope);
        match error {
            Some(e) => Err(e),
            None => Ok(result),
        }
    }

    /// Evaluate a single Value form and return a public EvalError on failure.
    /// Evaluate a single Value form, mapping Flow errors to EvalError.
    pub fn eval_form(&mut self, form: Value) -> Result<Value, EvalError> {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        self.eval_sub(form).map_err(map_flow)
    }

    /// Evaluate a runtime Value form, matching GNU Emacs's `eval_sub` in eval.c.
    ///
    /// Dispatch order (matching GNU eval.c:2552-2766):
    /// 1. Symbol → lexenv lookup or symbol-value
    /// 2. Non-cons → self-evaluating (return as-is)
    /// 3. Cons → special form / macro / function call
    pub fn eval_sub(&mut self, form: Value) -> EvalResult {
        // 1. Symbol → variable lookup (GNU eval.c:2554-2562)
        // Also unwrap symbol-with-pos when symbols-with-pos-enabled is true.
        let form_unwrapped = self.unwrap_symbol(form);
        if let Some(sym_id) = form_unwrapped.as_symbol_id() {
            return self.eval_symbol_by_id(sym_id);
        }

        // 2. Non-cons → self-evaluating (GNU eval.c:2564-2565)
        if !form_unwrapped.is_cons() {
            return Ok(form_unwrapped);
        }

        self.depth += 1;
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

        let result = self.maybe_grow_eval_stack(|ctx| {
            let specpdl_root_scope = ctx.save_specpdl_roots();
            ctx.push_specpdl_root(form);
            let result = ctx
                .maybe_gc_and_quit()
                .and_then(|()| ctx.eval_sub_cons(form));
            ctx.restore_specpdl_roots(specpdl_root_scope);
            result
        });
        self.depth -= 1;
        result
    }

    fn eval_sub_cons(&mut self, form: Value) -> EvalResult {
        let original_fun = self.unwrap_symbol(form.cons_car());
        let original_args = form.cons_cdr();

        // GNU eval.c:2583-2585 records an UNEVALLED backtrace frame on
        // every `eval_sub` cons-form evaluation. The frame starts in
        // UNEVALLED shape holding the surface function symbol and the
        // raw argument-form cons list, then transitions to EVALD in
        // place via `set_backtrace_args` once arguments have been
        // evaluated (eval.c:2638, 2660, 3299). Special forms leave
        // the frame UNEVALLED throughout.
        let outer_bt_count = self.specpdl.len();
        self.push_unevalled_backtrace_frame(original_fun, original_args);
        let result = self.eval_sub_cons_dispatch(original_fun, original_args, outer_bt_count);
        self.unbind_to(outer_bt_count);
        result
    }

    fn eval_sub_cons_dispatch(
        &mut self,
        original_fun: Value,
        original_args: Value,
        outer_bt_count: usize,
    ) -> EvalResult {
        // Resolve function (GNU eval.c:2600-2605)
        let sym_id = original_fun.as_symbol_id();

        // Keep only evaluator-internal literal forms on the pre-resolution
        // fast path. GNU decides public special-form dispatch from the
        // function cell's UNEVALLED subr, so user-visible special forms
        // should flow through the resolved subr surface below.
        if let Some(sym_id) = sym_id
            && matches!(
                sym_id,
                id if id == lambda_symbol()
                    || id == byte_code_literal_symbol()
                    || id == byte_code_symbol()
            )
        {
            if let Some(result) = self.try_special_form_value_id(sym_id, original_args) {
                return result;
            }
        }

        // Resolve function value
        let func = if let Some(sym_id) = sym_id {
            if let Some(override_func) =
                compiler_function_override_in_obarray(&self.obarray, sym_id)
            {
                override_func
            } else {
                match self.obarray.symbol_function_id(sym_id) {
                    Some(f) => {
                        let mut f = f;
                        // Follow symbol indirection (GNU eval.c:2604)
                        if let Some(alias_id) = f.as_symbol_id() {
                            if let Some(resolved) = self.obarray.indirect_function_id(alias_id) {
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
                                Some(reloaded) => reloaded,
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
            }
        } else if original_fun.is_cons() {
            // Car is a list — could be (lambda ...) form
            // Evaluate it to get the function
            self.eval_sub(original_fun)?
        } else {
            return Err(signal("invalid-function", vec![original_fun]));
        };

        if let Some(surface_sym_id) = sym_id
            && let Some(target_sym_id) = func.as_subr_id()
            && self.subr_is_special_form_id(target_sym_id)
        {
            // The outer eval_sub_cons UNEVALLED frame (pushed by the
            // wrapper) already records the surface function and raw
            // argument forms. Special forms leave the frame UNEVALLED
            // throughout (no `set_backtrace_args_evalled` call),
            // matching GNU eval.c:2618-2619.
            let result = if surface_sym_id == target_sym_id {
                self.try_special_form_value_id(surface_sym_id, original_args)
            } else {
                self.try_aliased_special_form_value_id(
                    surface_sym_id,
                    target_sym_id,
                    original_args,
                )
            };
            if let Some(result) = result {
                return result;
            }
        }

        // Check for macro (GNU eval.c:2730-2755)
        if func.is_macro() {
            let arg_values = value_list_to_values(&original_args);
            let bt_count = self.specpdl.len();
            self.push_backtrace_frame(original_fun, &arg_values);
            let expanded =
                self.with_macro_expansion_scope(|eval| eval.apply_lambda(func, arg_values));
            self.unbind_to(bt_count);
            // Evaluate expansion directly.
            return self.eval_sub(expanded?);
        }
        if cons_head_symbol_id(&func) == Some(macro_symbol()) {
            // Cons-cell macro: (macro . fn) — GNU eval.c:2730
            let macro_fn = func.cons_cdr();
            let arg_values = value_list_to_values(&original_args);
            let bt_count = self.specpdl.len();
            self.push_backtrace_frame(original_fun, &arg_values);
            let expanded = self.with_macro_expansion_scope(|eval| eval.apply(macro_fn, arg_values));
            self.unbind_to(bt_count);
            return self.eval_sub(expanded?);
        }

        // GNU eval.c:2606-2614: for SUBRP `fun`, check arity
        // against the raw `original_args` count BEFORE any arg
        // evaluation, and on mismatch signal
        // `(wrong-number-of-arguments original_fun numargs)` where
        // `original_fun` is the XCAR of the form (the surface
        // symbol, not the resolved subr value). This is how GNU
        // gets `(wrong-number-of-arguments car 0)` for a direct
        // `(car)` call -- the arity check runs inline in eval_sub
        // and never reaches `funcall_subr` which would have emitted
        // `#<subr car>` via `XSETSUBR`.
        //
        // For non-subrs (closures, bytecode, lambdas, cons forms)
        // the dispatch falls through to the normal apply path,
        // which signals with `fun` itself -- also matching GNU
        // funcall_lambda and funcall_subr.
        if let Some(sym_id) = func.as_subr_id()
            && let Some(entry) = lookup_global_subr_entry(sym_id)
            && entry.dispatch_kind != SubrDispatchKind::SpecialForm
        {
            let numargs = match list_length(&original_args) {
                Some(n) => n,
                None => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), original_args],
                    ));
                }
            };
            let min = entry.min_args as usize;
            let max_ok = match entry.max_args {
                Some(m) => numargs <= m as usize,
                None => true, // &rest / MANY
            };
            if numargs < min || !max_ok {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![original_fun, Value::fixnum(numargs as i64)],
                ));
            }
        }

        // GNU eval.c:2716-2726: when `fun` is not a subr, closure,
        // bytecode, or cons-shaped lambda/autoload/macro, signal
        // `(invalid-function original_fun)` with the SURFACE
        // symbol. Verified against emacs 31.0.50:
        //   (fset 'vm-fsetint 1)
        //   (condition-case e (vm-fsetint) (error e))
        //     → (invalid-function vm-fsetint)
        //
        // The check runs inline in eval_sub so the dispatcher
        // `funcall_general` never sees the invalid value and
        // never emits the resolved fncell contents as signal data.
        if !self.function_value_is_callable(&func) {
            if func.is_nil() {
                return Err(signal("void-function", vec![original_fun]));
            }
            return Err(signal("invalid-function", vec![original_fun]));
        }

        // Regular function call: evaluate args, promote the outer
        // UNEVALLED frame to EVALD in place, then dispatch directly.
        // Matches GNU `eval_sub` non-UNEVALLED SUBRP path
        // (eval.c:2631-2640) and CLOSUREP → apply_lambda
        // (eval.c:2715, 3292-3300) which both mutate the outer
        // record_in_backtrace entry via `set_backtrace_args`.
        //
        // `func` and each evaluated arg are rooted on the specpdl via
        // `push_specpdl_root`. GNU relies on conservative stack
        // scanning of `SAFE_ALLOCA_LISP (vals, numargs)` plus the
        // `fun` C local; neomacs uses exact GC, so a local
        // `Vec<Value>` and the Rust-local `func` Value are invisible
        // to the tracer.
        //
        // `func` is rooted BEFORE the arg loop so it survives GC
        // triggered by any arg evaluator, and stays rooted through
        // `funcall_general_untraced` below -- it only gets popped by
        // the outer `eval_sub_cons` `unbind_to(outer_bt_count)`. This
        // is specifically needed when `original_fun` is a cons
        // (lambda-literal head): the resolved Lambda Value lives only
        // on the Rust stack, and the outer UNEVALLED frame records
        // `original_fun`, not `func`.
        //
        // Per-arg roots are popped once `set_backtrace_args_evalled`
        // transfers ownership to the outer frame's args slot.
        let mut args = Vec::new();
        self.push_specpdl_root(func);
        let args_roots_base = self.specpdl.len();
        let mut cursor = original_args;
        while cursor.is_cons() {
            let arg_form = cursor.cons_car();
            let arg_val = self.eval_sub(arg_form)?;
            self.push_specpdl_root(arg_val);
            args.push(arg_val);
            cursor = cursor.cons_cdr();
        }
        self.set_backtrace_args_evalled(outer_bt_count, &args);
        self.unbind_to(args_roots_base);

        self.maybe_gc_and_quit()?;
        self.maybe_grow_eval_stack(|ctx| ctx.funcall_general_untraced(func, args))
    }

    /// Legacy eval_value: delegates to eval_sub.
    pub fn eval_value(&mut self, value: &Value) -> EvalResult {
        self.eval_sub(*value)
    }

    /// Evaluate all forms in a source string and return per-form results.
    /// Uses the Value-native reader.
    pub fn eval_str_each(&mut self, source: &str) -> Vec<Result<Value, EvalError>> {
        crate::tagged::gc::set_tagged_heap(&mut self.tagged_heap);
        let forms = match super::value_reader::read_all(source) {
            Ok(f) => f,
            Err(e) => {
                return vec![Err(EvalError::Signal {
                    symbol: intern("error"),
                    data: vec![Value::string(format!("Read error: {}", e.message))],
                    raw_data: None,
                })];
            }
        };
        // Root every parsed form upfront. The previous version only rooted
        // successful results; un-iterated parsed forms still sitting in the
        // heap-allocated Vec were otherwise invisible to exact GC.
        let specpdl_root_scope = self.save_specpdl_roots();
        for form in &forms {
            self.push_specpdl_root(*form);
        }
        let mut results = Vec::with_capacity(forms.len());
        for form in &forms {
            let result = self.eval_sub(*form).map_err(map_flow);
            if let Ok(ref val) = result {
                self.push_specpdl_root(*val);
            }
            results.push(result);
        }
        self.restore_specpdl_roots(specpdl_root_scope);
        results
    }

    /// Set a global variable.
    pub fn set_variable(&mut self, name: &str, value: Value) {
        let sym_id = intern(name);
        self.note_macro_expansion_mutation();
        // GNU set_internal (data.c:1762) for SYMBOL_FORWARDED routes
        // the write through `store_symval_forwarding` which for the
        // BUFFER_OBJFWD arm writes to the current buffer's slot.
        // Mirror that here so callers like
        // `obarray.set_symbol_value("default-directory", ...)`
        // (and the test surface that uses set_variable) actually
        // update the visible per-buffer slot rather than just the
        // obarray symbol value (which a FORWARDED symbol no longer
        // consults at read time).
        use super::symbol::SymbolRedirect;
        if let Some(sym) = self.obarray.get_by_id(sym_id)
            && sym.flags.redirect() == SymbolRedirect::Forwarded
            && let Some(buf_id) = self.buffers.current_buffer_id()
        {
            use super::forward::{LispBufferObjFwd, LispFwdType};
            // Safety: install_buffer_objfwd leaks a 'static
            // descriptor; the symbol's redirect tag and val.fwd
            // pointer are immutable once installed.
            let fwd_ptr = unsafe { sym.val.fwd };
            let header = unsafe { &*fwd_ptr };
            if matches!(header.ty, LispFwdType::BufferObj) {
                let buf_fwd = unsafe { &*(fwd_ptr as *const LispBufferObjFwd) };
                let offset = buf_fwd.offset as usize;
                if let Some(buf) = self.buffers.get_mut(buf_id)
                    && offset < buf.slots.len()
                {
                    buf.slots[offset] = value;
                    self.refresh_gc_runtime_settings_after_change_by_id(sym_id);
                    return;
                }
            }
        }
        self.obarray.set_symbol_value(name, value);
        self.sync_cached_runtime_binding_by_id(sym_id, value);
        self.refresh_gc_runtime_settings_after_change_by_id(sym_id);
    }

    #[inline]
    pub(crate) fn noninteractive(&self) -> bool {
        self.noninteractive
    }

    /// If `symbols-with-pos-enabled` and `val` is a symbol-with-pos,
    /// return the bare symbol. Otherwise return `val` unchanged.
    #[inline]
    pub fn unwrap_symbol(&self, val: Value) -> Value {
        if self.symbols_with_pos_enabled && val.is_symbol_with_pos() {
            val.as_symbol_with_pos_sym().unwrap()
        } else {
            val
        }
    }

    pub(crate) fn sync_thread_runtime_bindings(&mut self) {
        if let Some(main_thread) = self.threads.thread_handle(0) {
            self.obarray.set_symbol_value("main-thread", main_thread);
        }
    }

    /// Set a function binding.
    pub fn set_function(&mut self, name: &str, value: Value) {
        self.note_macro_expansion_mutation();
        self.obarray.set_symbol_function(name, value);
    }

    /// Look up a symbol by its SymId. Uses the SymId directly for lexenv
    /// lookup (preserving uninterned symbol identity, like Emacs's EQ-based
    /// Fassq on Vinternal_interpreter_environment).
    pub(crate) fn eval_symbol_by_id(&self, sym_id: SymId) -> EvalResult {
        // Keywords evaluate to themselves
        if is_keyword_id(sym_id) {
            return Ok(Value::from_kw_id(sym_id));
        }

        // GNU eval.c checks the lexenv for the ORIGINAL symbol BEFORE
        // resolving variable aliases and does not rescan declared-special
        // flags on ordinary reads. Declared-special affects how bindings are
        // created, not whether an existing lexical cell is readable.
        if self.lexical_binding() {
            if let Some(value) = self.lexenv_lookup_cached_in(self.lexenv, sym_id) {
                return Ok(value);
            }
        }

        let resolved = super::builtins::resolve_variable_alias_id(self, sym_id)?;
        let resolved_is_canonical = is_canonical_id(resolved);

        // Also check the lexenv for the resolved alias (rare but possible).
        if resolved != sym_id && self.lexical_binding() {
            if let Some(value) = self.lexenv_lookup_cached_in(self.lexenv, resolved) {
                return Ok(value);
            }
        }

        // Task #36: no t/nil short-circuit here. A lambda-parameter
        // or legitimate specbind can shadow the canonical constants
        // (GNU: `(funcall (lambda (t) t) 7)` → 7 even in dynamic
        // mode, because specbind stores 7 in the t symbol cell).
        // The fall-through to `find_symbol_value` below reads the
        // current cell; the canonical values are restored as a
        // fallback at the very end if no binding is found.
        if is_keyword_id(resolved) {
            return Ok(Value::from_kw_id(resolved));
        }

        // Phase 10E: route LOCALIZED reads through the BLV machinery
        // so they observe writes from `set_internal_localized` (vm.rs
        // assign_var_id and eval.rs set_runtime_binding). Without
        // this, the legacy `get_buffer_local_binding` lisp_bindings
        // fallback below returns stale data when the LOCALIZED hot
        // path bypassed it. Mirrors GNU `find_symbol_value` LOCALIZED
        // arm (`data.c:1620-1650`) — we use the immutable
        // `read_localized` here because `eval_symbol_by_id` takes
        // `&self` and can't run a mutable `swap_in_blv`.
        if resolved_is_canonical && let Some(buf) = self.buffers.current_buffer() {
            use crate::emacs_core::symbol::SymbolRedirect;
            if let Some(sym) = self.obarray.get_by_id(resolved)
                && sym.redirect() == SymbolRedirect::Localized
            {
                let target_buf = Value::make_buffer(buf.id);
                if let Some(value) =
                    self.obarray
                        .read_localized(resolved, target_buf, buf.local_var_alist)
                {
                    if value.is_unbound() {
                        return Err(signal("void-variable", vec![value_from_symbol_id(sym_id)]));
                    }
                    return Ok(value);
                }
            }
        }

        // Buffer-local bindings for FORWARDED BUFFER_OBJFWD slots: when
        // `make-local-variable` enables the per-buffer flag, reads must
        // return the slot value, not the default. Mirrors GNU
        // `find_symbol_value` (`data.c:1585`) routing FORWARDED reads
        // through `do_symval_forwarding` which reads the per-buffer slot
        // when its local_flags bit is set. Canonical symbols only — name-
        // based lookup must not intercept uninterned symbols sharing the
        // print name.
        if resolved_is_canonical && let Some(buf) = self.buffers.current_buffer() {
            if let Some(binding) = buf.get_buffer_local_binding_by_sym_id(resolved) {
                return binding
                    .as_value()
                    .ok_or_else(|| signal("void-variable", vec![value_from_symbol_id(sym_id)]));
            }
        }

        // Phase 10D: for FORWARDED BUFFER_OBJFWD symbols, route the
        // fall-through read through `BufferManager::buffer_defaults`
        // rather than the forwarder's static `default` field. The
        // static default is the install-time seed; `setq-default`
        // mutates `buffer_defaults` (which the forwarder descriptor
        // can't track because it lives in `'static` memory). Mirrors
        // GNU `do_symval_forwarding` BUFFER_OBJFWD reading from
        // either the per-buffer slot or `buffer_defaults`
        // (`data.c:1330-1352`).
        {
            use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
            use crate::emacs_core::symbol::SymbolRedirect;
            if let Some(sym) = self.obarray.get_by_id(resolved)
                && sym.redirect() == SymbolRedirect::Forwarded
            {
                let fwd = unsafe { &*sym.val.fwd };
                if matches!(fwd.ty, LispFwdType::BufferObj) {
                    let buf_fwd = unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                    let off = buf_fwd.offset as usize;
                    // Conditional slot with the per-buffer flag set
                    // already returned via get_buffer_local_binding
                    // above; this branch only fires when the bit is
                    // clear (or for always-local slots whose
                    // get_buffer_local_binding short-circuits to
                    // Some(...) — those won't reach here at all).
                    if off < self.buffers.buffer_defaults.len() {
                        return Ok(self.buffers.buffer_defaults[off]);
                    }
                    return Ok(buf_fwd.default);
                }
            }
        }

        // Obarray value cell. Use `find_symbol_value` (not the
        // legacy `symbol_value_id`) so FORWARDED reads land on the
        // forwarder descriptor's default rather than returning None
        // and signalling void-variable.
        if let Some(value) = self.obarray.find_symbol_value(resolved) {
            return Ok(value);
        }

        // Task #36: canonical constant fallback. When `t` / `nil`
        // aren't explicitly stored in the obarray and aren't
        // specbound, they resolve to their canonical values.
        // Mirrors the vm.rs `lookup_var` fallback path.
        if is_canonical_id(sym_id) && sym_id == nil_symbol() {
            return Ok(Value::NIL);
        }
        if is_canonical_id(sym_id) && sym_id == t_symbol() {
            return Ok(Value::T);
        }
        if resolved_is_canonical && resolved == nil_symbol() {
            return Ok(Value::NIL);
        }
        if resolved_is_canonical && resolved == t_symbol() {
            return Ok(Value::T);
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
            let invalid_fn = if self.subr_is_special_form_id(sym_id) {
                Value::subr_from_sym_id(sym_id)
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

        let Some(function) = self.obarray.symbol_function_id(sym_id) else {
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
            let invalid_fn = if self.subr_is_special_form_id(sym_id) {
                Value::subr_from_sym_id(sym_id)
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

        let Some(function) = self.obarray.symbol_function_id(sym_id) else {
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
            ValueKind::Subr(_) | ValueKind::Veclike(VecLikeType::Subr) => {
                super::subr_info::subr_is_callable_function_value(function)
            }
            ValueKind::Cons => {
                super::autoload::is_autoload_value(function)
                    || matches!(
                        cons_head_symbol_id(function),
                        Some(id) if is_lambda_like_symbol_id(id) || id == macro_symbol()
                    )
            }
            ValueKind::Symbol(id) => {
                super::builtins::symbols::resolve_indirect_symbol_by_id(self, id)
                    .is_some_and(|(_, resolved)| self.function_value_is_callable(&resolved))
            }
            _ => false,
        }
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

        if crate::emacs_core::value::equal_value(first_arg, &replacement, 0) {
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

        self.obarray.for_each_value_cell_mut(|value| {
            Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
        });
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
            ValueKind::Veclike(VecLikeType::Vector) => {
                let key = value.bits() ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut values = value.as_vector_data().unwrap().clone();
                for item in values.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                let _ = value.replace_vector_data(values);
            }
            ValueKind::Veclike(VecLikeType::Record) => {
                let key = value.bits() ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut values = value.as_record_data().unwrap().clone();
                for item in values.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                let _ = value.replace_record_data(values);
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
                let _ = value.replace_hash_table(ht);
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Special forms
    // -----------------------------------------------------------------------

    fn try_special_form_value_id(&mut self, sym_id: SymId, tail: Value) -> Option<EvalResult> {
        let saved_depth = self.depth;
        let result = self.try_special_form_inner_value_id(sym_id, tail);
        self.depth = saved_depth;
        result
    }

    fn try_aliased_special_form_value_id(
        &mut self,
        surface_id: SymId,
        target_id: SymId,
        tail: Value,
    ) -> Option<EvalResult> {
        let saved_depth = self.depth;
        let surface_name = resolve_sym(surface_id);
        let result = Some(match target_id {
            id if id == quote_symbol() => self.sf_quote_value_named(surface_name, tail),
            id if id == function_symbol() => self.sf_function_value_named(surface_name, tail),
            id if id == let_symbol() => self.sf_let_value_named(surface_name, tail),
            id if id == let_star_symbol() => self.sf_let_star_value_named(surface_name, tail),
            id if id == setq_symbol() => self.sf_setq_value_named(surface_name, tail),
            id if id == if_symbol() => self.sf_if_value_named(surface_name, tail),
            id if id == and_symbol() => self.sf_and_value(tail),
            id if id == or_symbol() => self.sf_or_value(tail),
            id if id == cond_symbol() => self.sf_cond_value(tail),
            id if id == while_symbol() => self.sf_while_value_named(surface_name, tail),
            id if id == progn_symbol() => self.sf_progn_value(tail),
            id if id == prog1_symbol() => self.sf_prog1_value_named(surface_name, tail),
            id if id == defvar_symbol() => self.sf_defvar_value_named(surface_name, tail),
            id if id == defconst_symbol() => self.sf_defconst_value_named(surface_name, tail),
            id if id == catch_symbol() => self.sf_catch_value_named(surface_name, tail),
            id if id == unwind_protect_symbol() => {
                self.sf_unwind_protect_value_named(surface_name, tail)
            }
            id if id == condition_case_symbol() => {
                self.sf_condition_case_value_named(surface_name, tail)
            }
            id if id == save_excursion_symbol() => self.sf_save_excursion_value(tail),
            id if id == save_current_buffer_symbol() => self.sf_save_current_buffer_value(tail),
            id if id == save_restriction_symbol() => self.sf_save_restriction_value(tail),
            id if id == interactive_symbol_id() => Ok(Value::NIL),
            _ => return None,
        });
        self.depth = saved_depth;
        result
    }

    fn try_special_form_inner_value_id(
        &mut self,
        sym_id: SymId,
        tail: Value,
    ) -> Option<EvalResult> {
        Some(match sym_id {
            id if id == quote_symbol() => self.sf_quote_value(tail),
            id if id == function_symbol() => self.sf_function_value(tail),
            id if id == let_symbol() => self.sf_let_value(tail),
            id if id == let_star_symbol() => self.sf_let_star_value(tail),
            id if id == setq_symbol() => self.sf_setq_value(tail),
            id if id == if_symbol() => self.sf_if_value(tail),
            id if id == and_symbol() => self.sf_and_value(tail),
            id if id == or_symbol() => self.sf_or_value(tail),
            id if id == cond_symbol() => self.sf_cond_value(tail),
            id if id == while_symbol() => self.sf_while_value(tail),
            id if id == progn_symbol() => self.sf_progn_value(tail),
            id if id == prog1_symbol() => self.sf_prog1_value(tail),
            id if id == defvar_symbol() => self.sf_defvar_value(tail),
            id if id == defconst_symbol() => self.sf_defconst_value(tail),
            id if id == catch_symbol() => self.sf_catch_value(tail),
            id if id == unwind_protect_symbol() => self.sf_unwind_protect_value(tail),
            id if id == condition_case_symbol() => self.sf_condition_case_value(tail),
            id if id == save_excursion_symbol() => self.sf_save_excursion_value(tail),
            id if id == save_current_buffer_symbol() => self.sf_save_current_buffer_value(tail),
            id if id == save_restriction_symbol() => self.sf_save_restriction_value(tail),
            id if id == interactive_symbol_id() => Ok(Value::NIL),
            id if id == lambda_symbol() => self.sf_lambda_value(tail),
            id if id == byte_code_literal_symbol() => self.sf_byte_code_literal_value(tail),
            id if id == byte_code_symbol() => self.sf_byte_code_value(tail),
            _ => return None,
        })
    }

    fn listp_error(&self, value: Value) -> Flow {
        // GNU `CHECK_LIST` walks the cdr chain until it finds the
        // non-cons tail and signals
        // `(wrong-type-argument listp TAIL)` with the offending
        // tail element, not the whole input. Verified against
        // emacs 31.0.50 via:
        //   (condition-case e (length '(1 . 2)) (error e))
        //     -> (wrong-type-argument listp 2)
        //   (condition-case e (let ((x 1) . 2) x) (error e))
        //     -> (wrong-type-argument listp 2)
        let mut tail = value;
        while tail.is_cons() {
            tail = tail.cons_cdr();
        }
        signal("wrong-type-argument", vec![Value::symbol("listp"), tail])
    }

    fn value_list_len_or_error(&self, list: Value) -> Result<usize, Flow> {
        list_length(&list).ok_or_else(|| self.listp_error(list))
    }

    fn one_unevalled_arg(&self, name: &str, tail: Value) -> Result<Value, Flow> {
        let mut cursor = tail;
        if !cursor.is_cons() {
            return if cursor.is_nil() {
                Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::symbol(name), Value::fixnum(0)],
                ))
            } else {
                Err(self.listp_error(tail))
            };
        }
        let arg = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if !cursor.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol(name),
                    Value::fixnum(self.value_list_len_or_error(tail)? as i64),
                ],
            ));
        }
        Ok(arg)
    }

    fn sf_quote_value(&mut self, tail: Value) -> EvalResult {
        self.sf_quote_value_named("quote", tail)
    }

    fn sf_quote_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        Ok(self.one_unevalled_arg(call_name, tail)?)
    }

    fn sf_function_value(&mut self, tail: Value) -> EvalResult {
        self.sf_function_value_named("function", tail)
    }

    fn sf_function_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        let arg = self.one_unevalled_arg(call_name, tail)?;
        if cons_head_symbol_id(&arg) == Some(lambda_symbol()) {
            return self.instantiate_callable_cons_form(arg);
        }
        Ok(arg)
    }

    fn sf_lambda_value(&mut self, tail: Value) -> EvalResult {
        self.instantiate_callable_cons_form(Value::cons(Value::from_sym_id(lambda_symbol()), tail))
    }

    fn sf_let_value(&mut self, tail: Value) -> EvalResult {
        self.sf_let_value_named("let", tail)
    }

    fn sf_let_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }

        let varlist = tail.cons_car();
        let body = tail.cons_cdr();
        let mut lexical_bindings: Vec<(SymId, Value)> = Vec::new();
        let mut dynamic_sym_ids: Vec<(SymId, Value)> = Vec::new();
        let use_lexical = self.lexical_binding();
        let mut constant_binding_error: Option<String> = None;
        let specpdl_root_scope = self.save_specpdl_roots();
        let mut bindings = varlist;

        while bindings.is_cons() {
            let binding = self.unwrap_symbol(bindings.cons_car());
            bindings = bindings.cons_cdr();
            if let Some(id) = binding.as_symbol_id() {
                if let Some(name) = symbol_sets_constant_error(id) {
                    if constant_binding_error.is_none() {
                        constant_binding_error = Some(name.to_owned());
                    }
                    continue;
                }
                if use_lexical
                    && !self.obarray.is_special_id(id)
                    && !self.lexenv_declares_special_cached_in(self.lexenv, id)
                {
                    lexical_bindings.push((id, Value::NIL));
                } else {
                    dynamic_sym_ids.push((id, Value::NIL));
                }
                continue;
            }
            if !binding.is_cons() {
                self.restore_specpdl_roots(specpdl_root_scope);
                return Err(signal("wrong-type-argument", vec![]));
            }
            let head = self.unwrap_symbol(binding.cons_car());
            let Some(id) = head.as_symbol_id() else {
                self.restore_specpdl_roots(specpdl_root_scope);
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), head],
                ));
            };
            let mut value_tail = binding.cons_cdr();
            let value = if value_tail.is_nil() {
                Value::NIL
            } else if value_tail.is_cons() {
                let init_form = value_tail.cons_car();
                value_tail = value_tail.cons_cdr();
                if !value_tail.is_nil() {
                    self.restore_specpdl_roots(specpdl_root_scope);
                    return Err(signal(
                        "error",
                        vec![
                            Value::string("`let' bindings can have only one value-form"),
                            binding,
                        ],
                    ));
                }
                match self.eval_sub(init_form) {
                    Ok(value) => value,
                    Err(err) => {
                        self.restore_specpdl_roots(specpdl_root_scope);
                        return Err(err);
                    }
                }
            } else {
                self.restore_specpdl_roots(specpdl_root_scope);
                return Err(self.listp_error(binding));
            };
            self.push_specpdl_root(value);
            if let Some(name) = symbol_sets_constant_error(id) {
                if constant_binding_error.is_none() {
                    constant_binding_error = Some(name.to_owned());
                }
                continue;
            }
            if use_lexical
                && !self.obarray.is_special_id(id)
                && !self.lexenv_declares_special_cached_in(self.lexenv, id)
            {
                lexical_bindings.push((id, value));
            } else {
                dynamic_sym_ids.push((id, value));
            }
        }
        if !bindings.is_nil() {
            self.restore_specpdl_roots(specpdl_root_scope);
            return Err(self.listp_error(varlist));
        }
        if let Some(name) = constant_binding_error {
            self.restore_specpdl_roots(specpdl_root_scope);
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        // CRITICAL: Restore specpdl roots (drop init-form GcRoot entries) BEFORE
        // pushing LexicalEnv/Let entries. Otherwise `restore_specpdl_roots`
        // drains from `saved_len` and re-extends with non-GcRoot entries,
        // MOVING our LexicalEnv to a lower index. Then `unbind_to(specpdl_count)`
        // becomes a no-op because specpdl.len() already matches, and the stale
        // LexicalEnv leaks below. This caused lexical binding leaks — closures
        // created in the body captured oversized environments.
        self.restore_specpdl_roots(specpdl_root_scope);

        // Save lexenv AFTER init forms run (matches GNU eval.c:1167:
        //   `lexenv = Vinternal_interpreter_environment;`).
        // Capture specpdl_count AFTER restoring so LexicalEnv sits exactly at
        // specpdl[specpdl_count] and unbind_to will pop it.
        let lexenv_at_entry = self.lexenv;
        let specpdl_count = self.specpdl.len();

        // Always save the entry-point lexenv on the specpdl when in lexical
        // mode, so unbind_to restores it regardless of what the body does.
        // Matches GNU's specbind(Qinternal_interpreter_environment).
        if use_lexical {
            self.specpdl.push(SpecBinding::LexicalEnv {
                old_lexenv: lexenv_at_entry,
            });
        }

        // Build new lexenv locally by consing bindings onto the ENTRY-POINT
        // lexenv (not self.lexenv which may have been modified by init forms).
        // Matches GNU eval.c:1167-1186.
        let mut new_lexenv = lexenv_at_entry;
        for (sym_id, val) in &lexical_bindings {
            let binding_pair = Value::make_cons(
                crate::emacs_core::eval::lexenv_binding_symbol_value(*sym_id),
                *val,
            );
            self.specpdl.push(SpecBinding::GcRoot { value: binding_pair });
            new_lexenv = Value::make_cons(binding_pair, new_lexenv);
            match self.specpdl.last_mut() {
                Some(SpecBinding::GcRoot { value }) => *value = new_lexenv,
                _ => unreachable!(),
            }
        }
        // Install the new lexenv atomically.
        self.lexenv = new_lexenv;
        for (sym_id, value) in &dynamic_sym_ids {
            self.specbind(*sym_id, *value);
        }

        let result = self.sf_progn_value(body);
        self.unbind_to(specpdl_count);
        result
    }

    fn sf_let_star_value(&mut self, tail: Value) -> EvalResult {
        self.sf_let_star_value_named("let*", tail)
    }

    fn sf_let_star_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }

        let varlist = tail.cons_car();
        let body = tail.cons_cdr();
        let use_lexical = self.lexical_binding();
        let specpdl_count = self.specpdl.len();
        // Mirrors GNU Flet_star: specbind(Qinternal_interpreter_environment, lexenv)
        // before any per-variable specbinds. unbind_to pops everything.
        if use_lexical {
            self.specpdl.push(SpecBinding::LexicalEnv {
                old_lexenv: self.lexenv,
            });
        }

        let init_result: Result<(), Flow> = (|| {
            let mut bindings = varlist;
            while bindings.is_cons() {
                let binding = self.unwrap_symbol(bindings.cons_car());
                bindings = bindings.cons_cdr();
                let (id, value) = if let Some(id) = binding.as_symbol_id() {
                    (id, Value::NIL)
                } else if binding.is_cons() {
                    let head = self.unwrap_symbol(binding.cons_car());
                    let Some(id) = head.as_symbol_id() else {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("symbolp"), head],
                        ));
                    };
                    let mut value_tail = binding.cons_cdr();
                    let value = if value_tail.is_nil() {
                        Value::NIL
                    } else if value_tail.is_cons() {
                        let init_form = value_tail.cons_car();
                        value_tail = value_tail.cons_cdr();
                        if !value_tail.is_nil() {
                            return Err(signal(
                                "error",
                                vec![
                                    Value::string("`let' bindings can have only one value-form"),
                                    binding,
                                ],
                            ));
                        }
                        self.eval_sub(init_form)?
                    } else {
                        return Err(self.listp_error(binding));
                    };
                    (id, value)
                } else {
                    return Err(signal("wrong-type-argument", vec![]));
                };

                if let Some(name) = symbol_sets_constant_error(id) {
                    return Err(signal("setting-constant", vec![Value::symbol(name)]));
                }
                if use_lexical
                    && !self.obarray.is_special_id(id)
                    && !self.lexenv_declares_special_cached_in(self.lexenv, id)
                {
                    // Matches GNU Flet_star (eval.c:1113-1120):
                    // Direct cons onto Vinternal_interpreter_environment.
                    // The LexicalEnv entry at specpdl_count saves the pre-let*
                    // state; unbind_to restores it.
                    let binding = Value::make_cons(
                        lexenv_binding_symbol_value(id),
                        value,
                    );
                    self.lexenv = Value::make_cons(binding, self.lexenv);
                } else {
                    self.specbind(id, value);
                }
            }
            if !bindings.is_nil() {
                return Err(self.listp_error(varlist));
            }
            Ok(())
        })();
        if let Err(error) = init_result {
            self.unbind_to(specpdl_count);
            return Err(error);
        }

        let result = self.sf_progn_value(body);
        self.unbind_to(specpdl_count);
        result
    }

    fn sf_setq_value(&mut self, tail: Value) -> EvalResult {
        self.sf_setq_value_named("setq", tail)
    }

    fn sf_setq_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Ok(Value::NIL);
        }
        let mut cursor = tail;
        let mut last = Value::NIL;
        let mut nargs: usize = 0;
        while cursor.is_cons() {
            let symbol = cursor.cons_car();
            cursor = cursor.cons_cdr();
            nargs += 1;
            if cursor.is_nil() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::symbol(call_name), Value::fixnum(nargs as i64)],
                ));
            }
            if !cursor.is_cons() {
                return Err(self.listp_error(tail));
            }
            let value_form = cursor.cons_car();
            cursor = cursor.cons_cdr();
            nargs += 1;
            let symbol = self.unwrap_symbol(symbol);
            let Some(sym_id) = symbol.as_symbol_id() else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), symbol],
                ));
            };
            let name = resolve_sym(sym_id);
            let value = self.eval_sub(value_form)?;
            let resolved = super::builtins::resolve_variable_alias_name(self, name)?;
            let resolved_id = intern(&resolved);
            if self.obarray.is_constant_id(resolved_id)
                && !self.has_local_binding_by_id(sym_id)
                && (resolved_id == sym_id || !self.has_local_binding_by_id(resolved_id))
            {
                return Err(signal("setting-constant", vec![Value::symbol(name)]));
            }
            // Debug probe for multibyte assignments to default-directory.
            // Kept at debug level so it doesn't pollute normal error
            // output (Doom always fires this with pure-ASCII paths that
            // happen to carry the multibyte flag from string decoding).
            if name == "default-directory" && value.is_string() && value.string_is_multibyte() {
                tracing::debug!(
                    "SETQ default-directory to MULTIBYTE string: {:?}",
                    runtime_string_value(value),
                );
            }
            if resolved != name {
                self.assign_with_watchers(&resolved, value, "set")?;
            } else {
                self.assign_with_watchers_by_id(sym_id, value, "set")?;
            }
            last = value;
        }
        if !cursor.is_nil() {
            return Err(self.listp_error(tail));
        }
        Ok(last)
    }

    fn sf_if_value(&mut self, tail: Value) -> EvalResult {
        self.sf_if_value_named("if", tail)
    }

    fn sf_if_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }
        let cond_form = tail.cons_car();
        let mut rest = tail.cons_cdr();
        if rest.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(1)],
            ));
        }
        if !rest.is_cons() {
            return Err(self.listp_error(tail));
        }
        let then_form = rest.cons_car();
        rest = rest.cons_cdr();
        if self.eval_sub(cond_form)?.is_truthy() {
            self.eval_sub(then_form)
        } else {
            self.sf_progn_value(rest)
        }
    }

    fn sf_and_value(&mut self, tail: Value) -> EvalResult {
        let mut cursor = tail;
        let mut last = Value::T;
        while cursor.is_cons() {
            last = self.eval_sub(cursor.cons_car())?;
            if last.is_nil() {
                return Ok(Value::NIL);
            }
            cursor = cursor.cons_cdr();
        }
        if !cursor.is_nil() {
            return Err(self.listp_error(tail));
        }
        Ok(last)
    }

    fn sf_or_value(&mut self, tail: Value) -> EvalResult {
        let mut cursor = tail;
        while cursor.is_cons() {
            let value = self.eval_sub(cursor.cons_car())?;
            if value.is_truthy() {
                return Ok(value);
            }
            cursor = cursor.cons_cdr();
        }
        if !cursor.is_nil() {
            return Err(self.listp_error(tail));
        }
        Ok(Value::NIL)
    }

    fn sf_cond_value(&mut self, tail: Value) -> EvalResult {
        let mut clauses = tail;
        while clauses.is_cons() {
            let clause = clauses.cons_car();
            clauses = clauses.cons_cdr();
            if clause.is_nil() {
                continue;
            }
            if !clause.is_cons() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), clause],
                ));
            }
            let test = clause.cons_car();
            let body = clause.cons_cdr();
            let test_value = self.eval_sub(test)?;
            if test_value.is_truthy() {
                if body.is_nil() {
                    return Ok(test_value);
                }
                return self.sf_progn_value(body);
            }
        }
        if !clauses.is_nil() {
            return Err(self.listp_error(tail));
        }
        Ok(Value::NIL)
    }

    fn sf_while_value(&mut self, tail: Value) -> EvalResult {
        self.sf_while_value_named("while", tail)
    }

    fn sf_while_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }
        let test_form = tail.cons_car();
        let body = tail.cons_cdr();
        let mut iters: u64 = 0;
        loop {
            if self.eval_sub(test_form)?.is_nil() {
                return Ok(Value::NIL);
            }
            self.sf_progn_value(body)?;
            iters += 1;
            if iters == 1_000_000 {
                let cond_str = super::print::print_value(&test_form);
                tracing::warn!(
                    "while loop exceeded 1M iterations, cond: {}",
                    &cond_str[..cond_str.len().min(300)]
                );
            }
            self.maybe_quit()?;
        }
    }

    fn sf_progn_value(&mut self, forms: Value) -> EvalResult {
        let mut cursor = forms;
        let mut last = Value::NIL;
        while cursor.is_cons() {
            last = self.eval_sub(cursor.cons_car())?;
            cursor = cursor.cons_cdr();
        }
        if !cursor.is_nil() {
            return Err(self.listp_error(forms));
        }
        Ok(last)
    }

    fn sf_prog1_value(&mut self, tail: Value) -> EvalResult {
        self.sf_prog1_value_named("prog1", tail)
    }

    fn sf_prog1_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }
        let first_form = tail.cons_car();
        let rest = tail.cons_cdr();
        let first = self.eval_sub(first_form)?;
        let specpdl_root_scope = self.save_specpdl_roots();
        self.push_specpdl_root(first);
        let result = self.sf_progn_value(rest);
        self.restore_specpdl_roots(specpdl_root_scope);
        result?;
        Ok(first)
    }

    fn sf_defvar_value(&mut self, tail: Value) -> EvalResult {
        self.sf_defvar_value_named("defvar", tail)
    }

    fn sf_defvar_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }

        let symbol = self.unwrap_symbol(tail.cons_car());
        let Some(sym_id) = symbol.as_symbol_id() else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), symbol],
            ));
        };
        let mut rest = tail.cons_cdr();

        if rest.is_nil() {
            if self.lexical_binding()
                && !self.lexenv.is_nil()
                && !self.obarray.is_special_id(sym_id)
            {
                self.lexenv = Value::cons(Value::from_sym_id(sym_id), self.lexenv);
            }
            return Ok(Value::from_sym_id(sym_id));
        }
        if !rest.is_cons() {
            return Err(self.listp_error(tail));
        }
        let init_form = rest.cons_car();
        rest = rest.cons_cdr();
        let documentation = if rest.is_nil() {
            Value::NIL
        } else if rest.is_cons() {
            let doc = rest.cons_car();
            rest = rest.cons_cdr();
            if !rest.is_nil() {
                return Err(signal("error", vec![Value::string("Too many arguments")]));
            }
            doc
        } else {
            return Err(self.listp_error(tail));
        };

        let mut define_args = vec![symbol];
        if !documentation.is_nil() {
            define_args.push(documentation);
        }
        super::builtins::symbols::builtin_internal_define_uninitialized_variable(
            self,
            define_args,
        )?;

        let was_bound =
            default_toplevel_value_in_state(&self.obarray, self.specpdl.as_slice(), sym_id)
                .is_some()
                || self.obarray.is_constant_id(sym_id);
        if !was_bound {
            let value = self.eval_sub(init_form)?;
            super::builtins::symbols::builtin_set_default_toplevel_value(
                self,
                vec![symbol, value],
            )?;
        }

        Ok(Value::from_sym_id(sym_id))
    }

    fn sf_defconst_value(&mut self, tail: Value) -> EvalResult {
        self.sf_defconst_value_named("defconst", tail)
    }

    fn sf_defconst_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }
        let symbol = self.unwrap_symbol(tail.cons_car());
        let Some(sym_id) = symbol.as_symbol_id() else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), symbol],
            ));
        };
        let mut rest = tail.cons_cdr();
        if rest.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(1)],
            ));
        }
        if !rest.is_cons() {
            return Err(self.listp_error(tail));
        }
        let init_form = rest.cons_car();
        rest = rest.cons_cdr();
        let documentation = if rest.is_nil() {
            Value::NIL
        } else if rest.is_cons() {
            let doc = rest.cons_car();
            rest = rest.cons_cdr();
            if !rest.is_nil() {
                return Err(signal("error", vec![Value::string("Too many arguments")]));
            }
            doc
        } else {
            return Err(self.listp_error(tail));
        };

        let mut define_args = vec![symbol];
        if !documentation.is_nil() {
            define_args.push(documentation);
        }
        super::builtins::symbols::builtin_internal_define_uninitialized_variable(
            self,
            define_args,
        )?;

        let value = self.eval_sub(init_form)?;
        super::custom::builtin_set_default(self, vec![symbol, value])?;
        self.obarray.make_special_id(sym_id);
        self.obarray
            .put_property_id(sym_id, intern("risky-local-variable"), Value::T)?;
        Ok(Value::from_sym_id(sym_id))
    }

    fn sf_catch_value(&mut self, tail: Value) -> EvalResult {
        self.sf_catch_value_named("catch", tail)
    }

    fn sf_catch_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(0)],
            ));
        }
        if !tail.is_cons() {
            return Err(self.listp_error(tail));
        }
        let tag = self.eval_sub(tail.cons_car())?;
        self.push_condition_frame(ConditionFrame::Catch {
            tag,
            resume: ResumeTarget::InterpreterCatch,
        });
        let result = match self.sf_progn_value(tail.cons_cdr()) {
            Ok(value) => Ok(value),
            Err(Flow::Throw {
                tag: thrown_tag,
                value,
            }) if eq_value(&tag, &thrown_tag) => Ok(value),
            Err(flow) => Err(flow),
        };
        self.pop_condition_frame();
        result
    }

    fn sf_unwind_protect_value(&mut self, tail: Value) -> EvalResult {
        self.sf_unwind_protect_value_named("unwind-protect", tail)
    }

    fn sf_unwind_protect_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        // GNU eval.c:1461 declares `unwind-protect` with min_args=1.
        // The generic arity check in GNU `eval_sub` (eval.c:2612) runs
        // for every SUBRP including UNEVALLED. Neomacs skips that check
        // for special forms (dispatch_kind != SpecialForm at
        // eval.rs:6599) so each special form validates itself -- see
        // `sf_condition_case_value_named`.
        let nargs = self.value_list_len_or_error(tail)?;
        if nargs < 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(nargs as i64)],
            ));
        }
        let body = tail.cons_car();
        let cleanup_forms = tail.cons_cdr();
        // Pre-allocate a `GcRoot` slot BELOW the `UnwindProtect` so
        // the body result is GC-rooted during cleanup. `unbind_to`
        // pops top-down, so when the `UnwindProtect` entry runs
        // cleanup the `GcRoot` slot beneath it is still on the stack
        // and visible to the tracer. GNU relies on conservative stack
        // scanning of a C local `val`; neomacs uses exact GC and
        // needs the value on specpdl.
        let root_slot = self.specpdl.len();
        self.specpdl.push(SpecBinding::GcRoot { value: Value::NIL });
        self.specpdl.push(SpecBinding::UnwindProtect {
            forms: cleanup_forms,
            lexenv: self.lexenv,
        });
        let result = self.eval_sub(body);
        if let Ok(v) = result {
            if let Some(SpecBinding::GcRoot { value }) = self.specpdl.get_mut(root_slot) {
                *value = v;
            }
        }
        self.unbind_to(root_slot);
        result
    }

    fn sf_condition_case_value(&mut self, tail: Value) -> EvalResult {
        self.sf_condition_case_value_named("condition-case", tail)
    }

    fn sf_condition_case_value_named(&mut self, call_name: &str, tail: Value) -> EvalResult {
        let nargs = self.value_list_len_or_error(tail)?;
        if nargs < 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol(call_name), Value::fixnum(nargs as i64)],
            ));
        }
        let var = self.unwrap_symbol(tail.cons_car());
        let Some(var_id) = var.as_symbol_id() else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), var],
            ));
        };
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return Err(self.listp_error(tail));
        }
        let body = rest.cons_car();
        let handlers = rest.cons_cdr();

        let mut handlers_vec = Vec::new();
        let mut success_handler_idx: Option<usize> = None;
        let mut cursor = handlers;
        while cursor.is_cons() {
            let handler = cursor.cons_car();
            let handler_index = handlers_vec.len();
            handlers_vec.push(handler);
            cursor = cursor.cons_cdr();
            if handler.is_nil() {
                continue;
            }
            if !handler.is_cons() {
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "Invalid condition handler: {}",
                        super::print::print_value(&handler)
                    ))],
                ));
            }
            let head = handler.cons_car();
            if !(head.is_symbol() || head.is_symbol_with_pos() || head.is_cons()) {
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "Invalid condition handler: {}",
                        super::print::print_value(&handler)
                    ))],
                ));
            }
            let head_unwrapped = self.unwrap_symbol(head);
            if head_unwrapped.is_symbol_named(":success") {
                success_handler_idx = Some(handler_index);
            }
        }
        if !cursor.is_nil() {
            return Err(self.listp_error(handlers));
        }

        let condition_stack_base = self.condition_stack_len();
        for (idx, handler) in handlers_vec.iter().enumerate().rev() {
            if success_handler_idx == Some(idx) || handler.is_nil() {
                continue;
            }
            if !handler.is_cons() {
                continue;
            }
            let conditions = handler.cons_car();
            self.push_condition_frame(ConditionFrame::ConditionCase {
                conditions,
                resume: ResumeTarget::InterpreterConditionCase {
                    handler_index: idx,
                    condition_stack_base,
                },
            });
        }

        match self.eval_sub(body) {
            Ok(value) => {
                self.truncate_condition_stack(condition_stack_base);
                if let Some(idx) = success_handler_idx {
                    let handler = handlers_vec[idx];
                    let bind_var = !var.is_nil();
                    let specpdl_count = self.specpdl.len();
                    if bind_var {
                        self.specbind(var_id, value);
                    }
                    let result = self.sf_progn_value(handler.cons_cdr());
                    self.unbind_to(specpdl_count);
                    return result;
                }
                Ok(value)
            }
            Err(Flow::Signal(sig)) => {
                let sig = match self.dispatch_signal_if_needed(sig) {
                    Ok(dispatched) => dispatched,
                    Err(flow) => {
                        self.truncate_condition_stack(condition_stack_base);
                        return Err(flow);
                    }
                };
                self.truncate_condition_stack(condition_stack_base);
                if let Some(ResumeTarget::InterpreterConditionCase {
                    handler_index,
                    condition_stack_base: selected_stack_base,
                }) = sig.selected_resume.clone()
                    && selected_stack_base == condition_stack_base
                {
                    let handler = handlers_vec[handler_index];
                    let bind_var = !var.is_nil();
                    let binding_value = make_signal_binding_value(&sig);
                    let use_lexical_binding = bind_var
                        && self.lexical_binding()
                        && !is_runtime_dynamically_special(&self.obarray, var_id)
                        && !self.lexenv_declares_special_cached_in(self.lexenv, var_id);

                    let specpdl_count = self.specpdl.len();
                    if use_lexical_binding {
                        // Match GNU: specbind the lexenv, then cons the
                        // binding directly.
                        self.specpdl.push(SpecBinding::LexicalEnv {
                            old_lexenv: self.lexenv,
                        });
                        let binding = Value::make_cons(
                            lexenv_binding_symbol_value(var_id),
                            binding_value,
                        );
                        self.lexenv = Value::make_cons(binding, self.lexenv);
                    } else if bind_var {
                        self.specbind(var_id, binding_value);
                    }
                    let result = self.sf_progn_value(handler.cons_cdr());
                    self.unbind_to(specpdl_count);
                    return result;
                }
                Err(Flow::Signal(sig))
            }
            Err(flow @ Flow::Throw { .. }) => {
                self.truncate_condition_stack(condition_stack_base);
                Err(flow)
            }
        }
    }

    fn sf_save_excursion_value(&mut self, tail: Value) -> EvalResult {
        let count = self.specpdl.len();
        if let Some(buf_id) = self.buffers.current_buffer().map(|b| b.id) {
            let pt = self.buffers.get(buf_id).map(|b| b.pt_byte).unwrap_or(0);
            let (marker_id, _marker_ptr) = self.buffers.create_marker(
                buf_id,
                pt,
                InsertionType::Before,
            );
            self.specpdl.push(SpecBinding::SaveExcursion {
                buffer_id: buf_id,
                marker_id,
            });
        }
        let result = self.sf_progn_value(tail);
        self.unbind_to(count);
        result
    }

    fn sf_save_current_buffer_value(&mut self, tail: Value) -> EvalResult {
        let saved_buf = self.buffers.current_buffer().map(|b| b.id);
        let result = self.sf_progn_value(tail);
        if let Some(saved_id) = saved_buf {
            self.restore_current_buffer_if_live(saved_id);
        }
        result
    }

    fn sf_save_restriction_value(&mut self, tail: Value) -> EvalResult {
        let count = self.specpdl.len();
        if let Some(state) = self.buffers.save_current_restriction_state() {
            self.specpdl.push(SpecBinding::SaveRestriction { state });
        }
        let result = self.sf_progn_value(tail);
        self.unbind_to(count);
        result
    }

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

    /// Recursively walk a `Value`, treating everything as literal data
    /// except `(byte-code-literal ...)` cons cells which are converted to
    /// `Value::ByteCode` via `sf_byte_code_literal_value`.
    fn quote_value_with_bytecode(&mut self, value: Value) -> EvalResult {
        if value.is_cons() && cons_head_symbol_id(&value) == Some(byte_code_literal_symbol()) {
            return self.sf_byte_code_literal_value(value.cons_cdr());
        }

        match value.kind() {
            ValueKind::Veclike(VecLikeType::Vector) => {
                let items = value.as_vector_data().unwrap();
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.quote_value_with_bytecode(*item)?);
                }
                Ok(Value::vector(values))
            }
            _ => Ok(value),
        }
    }

    fn sf_byte_code_literal_value(&mut self, tail: Value) -> EvalResult {
        let vector = self.one_unevalled_arg("byte-code-literal", tail)?;
        let Some(items) = vector.as_vector_data() else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("vectorp"), vector],
            ));
        };

        if items.len() < 4 {
            return Ok(vector);
        }

        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.quote_value_with_bytecode(*item)?);
        }

        crate::emacs_core::builtins::make_byte_code_from_parts(
            &values[0],
            &values[1],
            &values[2],
            &values[3],
            values.get(4),
            values.get(5),
        )
    }

    fn sf_byte_code_value(&mut self, tail: Value) -> EvalResult {
        let args = list_to_vec(&tail).ok_or_else(|| self.listp_error(tail))?;
        if args.len() != 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("byte-code"), Value::fixnum(args.len() as i64)],
            ));
        }
        let trace_toplevel_bytecode = std::env::var_os("NEOVM_TRACE_TOPLEVEL_BYTECODE").is_some();
        let load_file_name = if trace_toplevel_bytecode {
            self.obarray()
                .symbol_value("load-file-name")
                .and_then(|value| value.as_runtime_string_owned())
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            String::new()
        };
        let decode_start = trace_toplevel_bytecode.then(std::time::Instant::now);

        let bytecode_str = args[0];
        let constants_vec = self.quote_value_with_bytecode(args[1])?;
        let maxdepth = args[2];

        use crate::emacs_core::bytecode::ByteCodeFunction;
        use crate::emacs_core::bytecode::decode::{
            decode_gnu_bytecode_with_offset_map, string_value_to_bytes,
        };
        use crate::emacs_core::value::LambdaParams;

        // Bytecode strings are unibyte and may contain non-UTF-8 bytes.
        // Access raw bytes directly, same fix as make_byte_code_from_parts.
        let raw_bytes = if let Some(ls) = bytecode_str.as_lisp_string() {
            ls.as_bytes().to_vec()
        } else {
            Vec::new()
        };

        let mut constants: Vec<Value> = match constants_vec.kind() {
            ValueKind::Veclike(VecLikeType::Vector) => {
                constants_vec.as_vector_data().unwrap().clone()
            }
            _ => Vec::new(),
        };

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
            gnu_bytecode_bytes: None,
            docstring: None,
            doc_form: None,
            interactive: None,
        };

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
        result
    }

    pub(crate) fn defalias_value(&mut self, sym: Value, def: Value) -> EvalResult {
        let plan = builtins::plan_defalias_in_obarray(self.obarray(), &[sym, def])?;
        let builtins::DefaliasPlan { action, result, .. } = plan;
        match action {
            builtins::DefaliasAction::SetFunction { symbol, definition } => {
                self.note_macro_expansion_mutation();
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
        self.note_macro_expansion_mutation();
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
        let feature_name = super::builtins::symbols::symbol_id(&feature)
            .map(|sid| resolve_sym(sid).to_string());
        let filename_str = filename.as_ref().and_then(|v| v.as_runtime_string_owned());
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

    // -----------------------------------------------------------------------
    // Lambda / Function application
    // -----------------------------------------------------------------------

    fn maybe_use_cached_interpreted_closure_filter(
        &mut self,
        closure_hook: Value,
        params_value: Value,
        body_value: Value,
        env_value: Value,
        docstring_value: Value,
        iform_value: Value,
    ) -> Option<EvalResult> {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return None;
        };
        if hook_sym != cconv_make_interpreted_closure_symbol() {
            return None;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return None;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function_id(cconv_make_interpreted_closure_symbol())
        else {
            return None;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return None;
        }

        let env_shape = interpreted_closure_env_entries(env_value);
        let cache_fp =
            interpreted_closure_trim_fingerprint(params_value, body_value, iform_value, &env_shape);
        let entry = self
            .interpreted_closure_trim_cache
            .get(&cache_fp)?
            .iter()
            .find(|entry| entry.matches(params_value, body_value, iform_value, &env_shape))?
            .clone();
        let rebuilt_env =
            rebuild_trimmed_interpreted_closure_env(env_value, &entry.trimmed_env_template);
        Some(builtins::symbols::make_interpreted_closure_from_parts(
            &entry.trimmed_params_value,
            &entry.trimmed_body_value,
            &rebuilt_env,
            Some(&docstring_value),
            Some(&iform_value),
        ))
    }

    fn maybe_cache_interpreted_closure_filter_result(
        &mut self,
        closure_hook: Value,
        params_value: Value,
        body_value: Value,
        env_value: Value,
        iform_value: Value,
        result: &Value,
    ) {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return;
        };
        if hook_sym != cconv_make_interpreted_closure_symbol() {
            return;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function_id(cconv_make_interpreted_closure_symbol())
        else {
            return;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return;
        }
        if !result.is_lambda() {
            return;
        };
        let Some(trimmed_params_value) = result.closure_slot(CLOSURE_ARGLIST) else {
            return;
        };
        let Some(trimmed_body_value) = result.closure_body_value() else {
            return;
        };
        let Some(trimmed_env) = result.closure_env().flatten() else {
            return;
        };

        let env_shape = interpreted_closure_env_entries(env_value);
        let cache_fp =
            interpreted_closure_trim_fingerprint(params_value, body_value, iform_value, &env_shape);
        let bucket = self
            .interpreted_closure_trim_cache
            .entry(cache_fp)
            .or_default();
        if bucket
            .iter()
            .any(|entry| entry.matches(params_value, body_value, iform_value, &env_shape))
        {
            return;
        }
        bucket.push(InterpretedClosureTrimCacheEntry {
            params_value,
            body_value,
            iform_value,
            env_shape,
            trimmed_params_value,
            trimmed_body_value,
            trimmed_env_template: interpreted_closure_env_entries(trimmed_env),
        });
    }

    fn maybe_use_cached_value_interpreted_closure_filter(
        &mut self,
        closure_hook: Value,
        source_function: Value,
        env_value: Value,
        docstring_value: Value,
        iform_value: Value,
    ) -> Option<EvalResult> {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return None;
        };
        if hook_sym != cconv_make_interpreted_closure_symbol() {
            return None;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return None;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function_id(cconv_make_interpreted_closure_symbol())
        else {
            return None;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return None;
        }

        let env_shape = interpreted_closure_env_entries(env_value);
        let cache_key = (
            runtime_tail_fingerprint(&[source_function]),
            interpreted_closure_env_shape_hash(&env_shape),
        );
        let entry = self
            .interpreted_closure_value_cache
            .get(&cache_key)?
            .iter()
            .find(|entry| entry.matches(source_function, &env_shape))?
            .clone();
        let rebuilt_env =
            rebuild_trimmed_interpreted_closure_env(env_value, &entry.trimmed_env_template);
        Some(builtins::symbols::make_interpreted_closure_from_parts(
            &entry.trimmed_params_value,
            &entry.trimmed_body_value,
            &rebuilt_env,
            Some(&docstring_value),
            Some(&iform_value),
        ))
    }

    fn maybe_cache_value_interpreted_closure_filter_result(
        &mut self,
        closure_hook: Value,
        source_function: Value,
        env_value: Value,
        result: &Value,
    ) {
        let Some(hook_sym) = closure_hook.as_symbol_id() else {
            return;
        };
        if hook_sym != cconv_make_interpreted_closure_symbol() {
            return;
        }
        let Some(expected_fn) = self.interpreted_closure_filter_fn else {
            return;
        };
        let Some(current_fn) = self
            .obarray
            .symbol_function_id(cconv_make_interpreted_closure_symbol())
        else {
            return;
        };
        if !eq_value(&current_fn, &expected_fn) {
            return;
        }
        if !result.is_lambda() {
            return;
        };
        let Some(trimmed_params_value) = result.closure_slot(CLOSURE_ARGLIST) else {
            return;
        };
        let Some(trimmed_body_value) = result.closure_body_value() else {
            return;
        };
        let Some(trimmed_env) = result.closure_env().flatten() else {
            return;
        };

        let env_shape = interpreted_closure_env_entries(env_value);
        let cache_key = (
            runtime_tail_fingerprint(&[source_function]),
            interpreted_closure_env_shape_hash(&env_shape),
        );
        let bucket = self
            .interpreted_closure_value_cache
            .entry(cache_key)
            .or_default();
        if bucket
            .iter()
            .any(|entry| entry.matches(source_function, &env_shape))
        {
            return;
        }
        bucket.push(InterpretedClosureValueCacheEntry {
            source_function,
            env_shape,
            trimmed_params_value,
            trimmed_body_value,
            trimmed_env_template: interpreted_closure_env_entries(trimmed_env),
        });
    }

    fn make_interpreted_closure_with_expr_runtime_hook(
        &mut self,
        params_value: Value,
        body_value: Value,
        env_value: Value,
        docstring_value: Value,
        iform_value: Value,
    ) -> EvalResult {
        if !env_value.is_nil() {
            let closure_hook = self.visible_variable_value_or_nil_by_id(
                internal_make_interpreted_closure_function_symbol(),
            );
            if !closure_hook.is_nil() {
                if let Some(cached) = self.maybe_use_cached_interpreted_closure_filter(
                    closure_hook,
                    params_value,
                    body_value,
                    env_value,
                    docstring_value,
                    iform_value,
                ) {
                    return cached;
                }
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
                if let Ok(value) = &result {
                    self.maybe_cache_interpreted_closure_filter_result(
                        closure_hook,
                        params_value,
                        body_value,
                        env_value,
                        iform_value,
                        value,
                    );
                }
                return result;
            }
        }

        builtins::symbols::make_interpreted_closure_from_parts(
            &params_value,
            &body_value,
            &env_value,
            Some(&docstring_value),
            Some(&iform_value),
        )
    }

    fn make_interpreted_closure_with_value_runtime_hook(
        &mut self,
        source_function: Value,
        params_value: Value,
        body_value: Value,
        env_value: Value,
        docstring_value: Value,
        iform_value: Value,
    ) -> EvalResult {
        if !env_value.is_nil() {
            let closure_hook = self.visible_variable_value_or_nil_by_id(
                internal_make_interpreted_closure_function_symbol(),
            );
            if !closure_hook.is_nil() {
                if let Some(cached) = self.maybe_use_cached_value_interpreted_closure_filter(
                    closure_hook,
                    source_function,
                    env_value,
                    docstring_value,
                    iform_value,
                ) {
                    return cached;
                }
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
                if let Ok(value) = &result {
                    self.maybe_cache_value_interpreted_closure_filter_result(
                        closure_hook,
                        source_function,
                        env_value,
                        value,
                    );
                }
                return result;
            }
        }

        builtins::symbols::make_interpreted_closure_from_parts(
            &params_value,
            &body_value,
            &env_value,
            Some(&docstring_value),
            Some(&iform_value),
        )
    }

    fn eval_dynamic_documentation_value(&mut self, value: Value) -> Result<Option<Value>, Flow> {
        if !value.is_cons() || value.cons_car().as_symbol_name() != Some(":documentation") {
            return Ok(None);
        }

        let tail = value.cons_cdr();
        if tail.is_nil() {
            return Ok(Some(Value::NIL));
        }
        if !tail.is_cons() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), value],
            ));
        }

        self.eval_value(&tail.cons_car()).map(Some)
    }

    pub(crate) fn push_backtrace_frame(&mut self, function: Value, args: &[Value]) {
        self.specpdl.push(SpecBinding::Backtrace {
            function,
            args: args.iter().copied().collect(),
            debug_on_exit: false,
            unevalled: false,
        });
    }

    /// Push a backtrace frame for a special-form call (`nargs == UNEVALLED`
    /// in GNU eval.c:2585). `original_args` is the cons list of un-evaluated
    /// argument forms — XCDR of the original form. The walker emits
    /// `(nil FUNC FORMS FLAGS)` for these frames.
    pub(crate) fn push_unevalled_backtrace_frame(
        &mut self,
        function: Value,
        original_args: Value,
    ) {
        let mut args = LispArgVec::new();
        args.push(original_args);
        self.specpdl.push(SpecBinding::Backtrace {
            function,
            args,
            debug_on_exit: false,
            unevalled: true,
        });
    }

    /// Promote the UNEVALLED backtrace frame at `specpdl[count]` to the
    /// EVALD shape in place. Mirrors GNU `set_backtrace_args`
    /// (eval.c:144-156) called at eval.c:2638, 2660, 3299 after
    /// argument evaluation completes.
    ///
    /// `count` is the `specpdl.len()` observed *before* the outer
    /// `push_unevalled_backtrace_frame` — the same value a caller
    /// would pass to `unbind_to`.
    ///
    /// Panics if the slot is not an UNEVALLED backtrace frame. Callers
    /// must keep the invariant that every `set_backtrace_args_evalled`
    /// matches exactly one prior `push_unevalled_backtrace_frame`.
    pub(crate) fn set_backtrace_args_evalled(&mut self, count: usize, evaluated: &[Value]) {
        let entry = self
            .specpdl
            .get_mut(count)
            .expect("set_backtrace_args_evalled: specpdl index out of range");
        match entry {
            SpecBinding::Backtrace {
                unevalled,
                args,
                ..
            } if *unevalled => {
                args.clear();
                args.extend(evaluated.iter().copied());
                *unevalled = false;
            }
            other => panic!(
                "set_backtrace_args_evalled: expected UNEVALLED Backtrace at specpdl[{count}], got {other:?}"
            ),
        }
    }


    pub(crate) fn save_specpdl_roots(&self) -> SpecpdlRootScopeState {
        SpecpdlRootScopeState {
            saved_len: self.specpdl.len(),
        }
    }

    pub(crate) fn push_specpdl_root(&mut self, value: Value) {
        self.specpdl.push(SpecBinding::GcRoot { value });
    }

    pub(crate) fn restore_specpdl_roots(&mut self, scope: SpecpdlRootScopeState) {
        if self.specpdl.len() <= scope.saved_len {
            return;
        }
        let mut tail: Vec<SpecBinding> = self.specpdl.drain(scope.saved_len..).collect();
        self.specpdl.extend(
            tail.drain(..)
                .filter(|binding| !matches!(binding, SpecBinding::GcRoot { .. })),
        );
    }
    pub(crate) fn push_vm_root_frame(&mut self) {
        self.vm_root_frames.push(VmRootFrame::new());
    }

    pub(crate) fn pop_vm_root_frame(&mut self) {
        self.vm_root_frames.pop();
    }

    pub(crate) fn push_vm_frame_root(&mut self, value: Value) {
        self.vm_root_frames
            .last_mut()
            .expect("VM root frame missing")
            .roots
            .push(value);
    }

    pub(crate) fn save_vm_roots(&mut self) -> VmRootScopeState {
        let pushed_vm_root_frame = self.vm_root_frames.is_empty();
        if pushed_vm_root_frame {
            self.push_vm_root_frame();
        }
        VmRootScopeState {
            pushed_vm_root_frame,
            saved_vm_root_frame_len: self.vm_root_frames.last().map(|frame| frame.roots.len()),
        }
    }

    pub(crate) fn save_vm_frame_roots(&self) -> usize {
        self.vm_root_frames
            .last()
            .expect("VM root frame missing")
            .roots
            .len()
    }

    pub(crate) fn restore_vm_frame_roots(&mut self, saved_len: usize) {
        self.vm_root_frames
            .last_mut()
            .expect("VM root frame missing")
            .roots
            .truncate(saved_len);
    }

    pub(crate) fn restore_vm_roots(&mut self, scope: VmRootScopeState) {
        if let Some(saved_len) = scope.saved_vm_root_frame_len {
            self.restore_vm_frame_roots(saved_len);
        }
        if scope.pushed_vm_root_frame {
            self.pop_vm_root_frame();
        }
    }

    fn apply_internal(
        &mut self,
        function: Value,
        args: Vec<Value>,
        record_backtrace: bool,
    ) -> EvalResult {
        let bt_count = self.specpdl.len();
        if record_backtrace {
            self.push_backtrace_frame(function, &args);
        }
        let result = self.maybe_gc_and_quit().and_then(|_| {
            // GNU does not probe stack space for every funcall. Keep growth
            // checks at the function-application boundary, but only on coarse
            // depth intervals so normal startup is not dominated by TLS lookups
            // in stacker::maybe_grow.
            self.maybe_grow_eval_stack(|ctx| {
                ctx.funcall_general_untraced(function, args)
            })
        });
        self.unbind_to(bt_count);
        result
    }

    /// Apply a function value to evaluated arguments.
    pub(crate) fn apply(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        self.apply_internal(function, args, true)
    }

    pub(crate) fn apply_untraced(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        self.apply_internal(function, args, false)
    }

    /// Apply FUNC to ARGS, but record FRAME_FUNCTION (not FUNC) in the
    /// runtime backtrace frame. Used by `eval_sub_cons` when the form
    /// dispatches through a symbol: the symbol is what GNU stores in
    /// specpdl (and what `backtrace-frame` returns), while the
    /// resolved function cell is what actually runs.
    ///
    /// Mirrors GNU's `eval_sub` SYMBOLP arm at `eval.c:2600-2625`,
    /// where `original_fun` (the symbol) is the value written into the
    /// specpdl entry via `record_in_backtrace (original_fun, args, ...)`.
    pub(crate) fn apply_with_frame_function(
        &mut self,
        frame_function: Value,
        func: Value,
        args: Vec<Value>,
    ) -> EvalResult {
        let bt_count = self.specpdl.len();
        self.push_backtrace_frame(frame_function, &args);
        let result = self.maybe_gc_and_quit().and_then(|_| {
            self.maybe_grow_eval_stack(|ctx| {
                ctx.funcall_general_untraced(func, args)
            })
        });
        self.unbind_to(bt_count);
        result
    }

    /// Unified function dispatch — matches GNU Emacs's funcall_general.
    /// Called by both the tree-walking interpreter (via apply) and the
    /// bytecode VM (via Vm::call_function).
    pub(crate) fn funcall_general(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        let bt_count = self.specpdl.len();
        self.push_backtrace_frame(function, &args);
        let result = self.funcall_general_untraced(function, args);
        self.unbind_to(bt_count);
        result
    }

    pub(crate) fn funcall_general_untraced(
        &mut self,
        function: Value,
        args: Vec<Value>,
    ) -> EvalResult {
        match function.kind() {
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                // get_bytecode_data returns a reference into the GC-managed
                // ByteCodeObj.  GNU's bytecode interpreter executes from the
                // function struct in place, never copying.  Don't clone here
                // either — bytecode functions can have thousands of ops, and
                // cloning per call dominated debug-build batch-byte-compile
                // runtime.
                let bc_data = function.get_bytecode_data().unwrap();
                let mut vm = super::bytecode::Vm::from_context(self);
                vm.execute_with_func_value(bc_data, args, function)
            }
            ValueKind::Veclike(VecLikeType::Lambda) => self.apply_lambda(function, args),
            ValueKind::Veclike(VecLikeType::Macro) => self.apply_lambda(function, args),
            ValueKind::Subr(_) => self.apply_subr_object(function, args, true),
            ValueKind::Veclike(VecLikeType::Subr) => self.apply_subr_object(function, args, true),
            ValueKind::Symbol(id) => self.apply_symbol_callable_untraced(id, args, true),
            ValueKind::T => self.apply_symbol_callable_untraced(intern("t"), args, true),
            ValueKind::Nil => Err(signal("void-function", vec![Value::symbol("nil")])),
            _ if function.is_symbol_with_pos() => {
                // Transparently unwrap symbol-with-pos → bare symbol for funcall dispatch.
                let bare = function.as_symbol_with_pos_sym().unwrap();
                self.funcall_general_untraced(bare, args)
            }
            ValueKind::Cons => {
                if super::autoload::is_autoload_value(&function) {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), function],
                    ))
                } else if matches!(
                    cons_head_symbol_id(&function),
                    Some(id) if is_lambda_like_symbol_id(id)
                ) {
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
        // Unwrap symbol-with-pos on the car so (lambda ...) / (closure ...)
        // forms with position-wrapped heads are recognized.
        let head_val = items.first().map(|v| self.unwrap_symbol(*v));
        let Some(head_name) = head_val.and_then(|v| v.as_symbol_name()) else {
            return Err(signal("invalid-function", vec![function]));
        };

        let (env_value, params_value, mut body_start) = match head_name {
            "lambda" => {
                let Some(params_value) = items.get(1).copied() else {
                    return Err(signal("invalid-function", vec![function]));
                };
                // Mirrors GNU eval_sub lambda handling: a lambda gets
                // a lexical closure env only when
                // Vinternal_interpreter_environment is non-nil (i.e.
                // lexical mode is active). We use self.lexenv as the
                // single source of truth, matching GNU.
                let env_value = if !self.lexenv.is_nil() {
                    self.lexenv
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

        let specpdl_root_scope = self.save_specpdl_roots();
        self.push_specpdl_root(function);

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
        if let Some(item) = items.get(body_start).copied()
            && let Some(doc_form) = self.eval_dynamic_documentation_value(item)?
        {
            doc_form_value = doc_form;
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

        let mut iform_value = Value::NIL;
        if items.get(body_start).is_some_and(|value| {
            value.is_cons() && value.cons_car().as_symbol_name() == Some("interactive")
        }) {
            iform_value = items[body_start];
            body_start += 1;
        }

        let body_value = if body_start >= items.len() {
            Value::list(vec![Value::NIL])
        } else {
            Value::list(items[body_start..].to_vec())
        };
        let closure_doc_value = if !doc_form_value.is_nil() {
            doc_form_value
        } else {
            docstring_value
        };

        self.push_specpdl_root(params_value);
        self.push_specpdl_root(body_value);
        self.push_specpdl_root(env_value);
        self.push_specpdl_root(closure_doc_value);
        self.push_specpdl_root(iform_value);

        let result = if head_name == "lambda" {
            self.make_interpreted_closure_with_value_runtime_hook(
                function,
                params_value,
                body_value,
                env_value,
                closure_doc_value,
                iform_value,
            )
        } else {
            builtins::symbols::make_interpreted_closure_from_parts(
                &params_value,
                &body_value,
                &env_value,
                Some(&closure_doc_value),
                Some(&iform_value),
            )
        };
        self.restore_specpdl_roots(specpdl_root_scope);
        result
    }

    /// GNU funcall_subr (eval.c:3266-3280) pre-checks arity and
    /// signals `(wrong-number-of-arguments #<subr NAME> NUMARGS)`
    /// with the SUBR value. Call this before dispatching to a
    /// builtin so the check matches GNU's `funcall_subr` exactly
    /// and we never depend on the builtin's expect_args helper
    /// (which would emit `Value::symbol(name)` instead of the
    /// subr value).
    ///
    /// Returns `Some(Flow::Signal)` on arity mismatch, `None` when
    /// the arity is acceptable or the subr has no explicit arity
    /// registered (opt-out).
    #[inline]
    fn check_funcall_subr_arity_value(&self, function: Value, nargs: usize) -> Option<Flow> {
        let sym_id = function.as_subr_id()?;
        let entry = lookup_global_subr_entry(sym_id)?;
        let min = entry.min_args as usize;
        let max = entry.max_args.map(|m| m as usize);
        // Opt-out: a subr registered with (0, None) has declared
        // "I do my own arity check". Keep the legacy behaviour for
        // those until each one is migrated explicitly.
        if min == 0 && max.is_none() {
            return None;
        }
        let arity_bad = nargs < min || max.is_some_and(|m| nargs > m);
        if arity_bad {
            Some(signal(
                "wrong-number-of-arguments",
                vec![function, Value::fixnum(nargs as i64)],
            ))
        } else {
            None
        }
    }

    #[inline]
    fn check_funcall_subr_arity(&self, sym_id: SymId, nargs: usize) -> Option<Flow> {
        self.check_funcall_subr_arity_value(Value::subr_from_sym_id(sym_id), nargs)
    }

    fn dispatch_subr_value_internal(
        &mut self,
        function: Value,
        args: Vec<Value>,
        wrong_arity_callee: Value,
    ) -> Option<EvalResult> {
        let sym_id = function.as_subr_id()?;
        let entry = lookup_global_subr_entry(sym_id)?;
        let func = entry.function?;
        let name = resolve_name(entry.name_id);
        if name == "cdr" && args.len() == 1 && args[0].is_t() {
            tracing::error!("(cdr t) called! Lisp backtrace:");
            for (i, bt_entry) in self.specpdl.iter().rev()
                .filter_map(|e| match e {
                    SpecBinding::Backtrace { function, .. } => Some(function),
                    _ => None,
                })
                .take(10)
                .enumerate()
            {
                let func_name = super::print::print_value(bt_entry);
                tracing::error!("  bt[{}]: {}", i, func_name);
            }
        }
        let nargs = args.len();
        if (nargs as u16) < entry.min_args {
            return Some(Err(signal(
                "wrong-number-of-arguments",
                vec![wrong_arity_callee, Value::fixnum(nargs as i64)],
            )));
        }
        if let Some(max) = entry.max_args {
            if nargs as u16 > max {
                return Some(Err(signal(
                    "wrong-number-of-arguments",
                    vec![wrong_arity_callee, Value::fixnum(nargs as i64)],
                )));
            }
        }
        Some(match func {
            crate::tagged::header::SubrFn::Many(func) => func(self, args),
            crate::tagged::header::SubrFn::A0(func) => func(self),
            crate::tagged::header::SubrFn::A1(func) => {
                func(self, args.first().copied().unwrap_or(Value::NIL))
            }
            crate::tagged::header::SubrFn::A2(func) => func(
                self,
                args.first().copied().unwrap_or(Value::NIL),
                args.get(1).copied().unwrap_or(Value::NIL),
            ),
        })
    }

    #[inline]
    fn apply_subr_object(
        &mut self,
        function: Value,
        args: Vec<Value>,
        _rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let Some(sym_id) = function.as_subr_id() else {
            return Err(signal("invalid-function", vec![function]));
        };
        let Some(entry) = lookup_global_subr_entry(sym_id) else {
            return Err(signal("invalid-function", vec![function]));
        };
        if entry.dispatch_kind == SubrDispatchKind::SpecialForm {
            return Err(signal("invalid-function", vec![function]));
        }
        if entry.dispatch_kind == SubrDispatchKind::ContextCallable {
            return self.apply_evaluator_callable_by_id(sym_id, args);
        }
        if let Some(flow) = self.check_funcall_subr_arity_value(function, args.len()) {
            return Err(flow);
        }
        if let Some(result) = self.dispatch_subr_value_internal(function, args, function) {
            result.map_err(|flow| self.validate_throw(flow))
        } else {
            Err(signal("void-function", vec![Value::from_sym_id(sym_id)]))
        }
    }

    #[inline]
    fn resolve_named_call_target_by_id(&mut self, sym_id: SymId) -> NamedCallTarget {
        let compiler_overrides_active =
            compiler_function_overrides_active_in_obarray(&self.obarray);
        let function_epoch = self.obarray.function_epoch();
        if !compiler_overrides_active {
            // Fast path: a HashMap lookup that returns the cached target
            // when the function epoch hasn't moved.  An epoch mismatch
            // signals that some `defalias`/`fset`/autoload installation
            // happened since the cached entry was recorded; in that case
            // fall through and replace the entry below.
            if let Some(entry) = self.named_call_cache.get(&sym_id)
                && entry.function_epoch == function_epoch
            {
                return entry.target.clone();
            }
        }

        let target =
            if let Some(func) = compiler_function_override_in_obarray(&self.obarray, sym_id) {
                NamedCallTarget::Obarray(func)
            } else if let Some(func) = self.obarray.symbol_function_id(sym_id) {
                match func.kind() {
                    ValueKind::Nil => NamedCallTarget::Void,
                    // `(fset 'foo (symbol-function 'foo))` writes `#<subr foo>` into
                    // the function cell. Treat this as the canonical callable
                    // object, not an obarray indirection cycle.
                    ValueKind::Subr(sid) if sid == sym_id => {
                        NamedCallTarget::Subr(Value::subr_from_sym_id(sid))
                    }
                    ValueKind::Veclike(VecLikeType::Subr) if func.as_subr_id() == Some(sym_id) => {
                        NamedCallTarget::Subr(func)
                    }
                    _ => NamedCallTarget::Obarray(func),
                }
            } else if self.obarray.is_function_unbound_id(sym_id) {
                NamedCallTarget::Void
            } else if lookup_global_subr_entry(sym_id).is_some() {
                NamedCallTarget::Subr(Value::subr_from_sym_id(sym_id))
            } else {
                NamedCallTarget::Void
            };

        if !compiler_overrides_active {
            // Cap the cache to avoid unbounded growth on pathologic
            // workloads.  Past the cap we just stop caching new entries
            // — better to take an O(1) miss than to evict a hot entry.
            if self.named_call_cache.len() < NAMED_CALL_CACHE_CAPACITY {
                self.named_call_cache.insert(
                    sym_id,
                    NamedCallCacheEntry {
                        function_epoch,
                        target: target.clone(),
                    },
                );
            }
        }

        target
    }

    #[inline]
    fn resolve_named_call_target(&mut self, name: &str) -> NamedCallTarget {
        self.resolve_named_call_target_by_id(intern(name))
    }

    #[inline]
    fn store_named_call_cache(&mut self, symbol: SymId, target: NamedCallTarget) {
        let function_epoch = self.obarray.function_epoch();
        if self.named_call_cache.len() < NAMED_CALL_CACHE_CAPACITY {
            self.named_call_cache.insert(
                symbol,
                NamedCallCacheEntry {
                    function_epoch,
                    target,
                },
            );
        }
    }

    #[inline]
    fn apply_named_callable_by_id(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        let frame_function = Value::from_sym_id(sym_id);
        let bt_count = self.specpdl.len();
        self.push_backtrace_frame(frame_function, &args);
        let result = self.apply_named_callable_by_id_core(
            sym_id,
            args,
            invalid_fn,
            rewrite_builtin_wrong_arity,
        );
        self.unbind_to(bt_count);
        result
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
        let bt_count = self.specpdl.len();
        self.push_backtrace_frame(frame_function, &args);
        let result =
            self.apply_named_callable_core(name, args, invalid_fn, rewrite_builtin_wrong_arity);
        self.unbind_to(bt_count);
        result
    }

    fn apply_named_callable_by_id_core(
        &mut self,
        sym_id: SymId,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        match self.resolve_named_call_target_by_id(sym_id) {
            NamedCallTarget::Obarray(func) => {
                if super::autoload::is_autoload_value(&func) {
                    return self.apply_named_autoload_callable_by_id(
                        sym_id,
                        func,
                        args,
                        rewrite_builtin_wrong_arity,
                    );
                }
                let function_is_callable = self.function_value_is_callable(&func);
                let result = match self.apply_untraced(func, args) {
                    Err(Flow::Signal(sig))
                        if sig.symbol_name() == "invalid-function" && !function_is_callable =>
                    {
                        Err(signal("invalid-function", vec![Value::from_sym_id(sym_id)]))
                    }
                    other => other,
                };
                result
            }
            NamedCallTarget::Subr(func) => {
                let result = self.apply_subr_object(func, args, rewrite_builtin_wrong_arity);
                // Do NOT poison the cache with Void when the subr was found.
                // A void-function from a known subr is transient (e.g., dispatch
                // failure during initialization), not a permanent state change.
                if func.as_subr_id()
                    .and_then(lookup_global_subr_entry)
                    .is_some_and(|e| e.dispatch_kind == SubrDispatchKind::SpecialForm)
                {
                    Err(signal("invalid-function", vec![invalid_fn]))
                } else {
                    result
                }
            }
            NamedCallTarget::Void => Err(signal("void-function", vec![Value::from_sym_id(sym_id)])),
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
                let result = match self.apply(func, args) {
                    Err(Flow::Signal(sig))
                        if sig.symbol_name() == "invalid-function" && !function_is_callable =>
                    {
                        Err(signal("invalid-function", vec![Value::symbol(name)]))
                    }
                    other => other,
                };
                result
            }
            NamedCallTarget::Subr(func) => {
                let sym_id = intern(name);
                let result = self.apply_subr_object(func, args, rewrite_builtin_wrong_arity);
                // Do NOT poison the cache with Void when the subr was found.
                if func.as_subr_id()
                    .and_then(lookup_global_subr_entry)
                    .is_some_and(|e| e.dispatch_kind == SubrDispatchKind::SpecialForm)
                {
                    Err(signal("invalid-function", vec![invalid_fn]))
                } else {
                    result
                }
            }
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
        self.apply_named_autoload_callable_by_id(
            intern(name),
            autoload_form,
            args,
            rewrite_builtin_wrong_arity,
        )
    }

    fn apply_named_autoload_callable_by_id(
        &mut self,
        sym_id: SymId,
        autoload_form: Value,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        // Startup wrappers often expose autoload-shaped function cells for names
        // backed by builtins. Keep the autoload shape while preserving callability.
        if lookup_global_subr_entry(sym_id).is_some() {
            let subr = Value::subr_from_sym_id(sym_id);
            // GNU-faithful pre-check via check_funcall_subr_arity.
            if let Some(flow) = self.check_funcall_subr_arity_value(subr, args.len()) {
                return Err(flow);
            }
            if let Some(result) =
                self.dispatch_subr_value_internal(subr, args.clone(), Value::subr_from_sym_id(sym_id))
            {
                return result;
            }
        }

        let loaded = super::autoload::builtin_autoload_do_load(
            self,
            vec![autoload_form, Value::from_sym_id(sym_id)],
        )?;
        let function_is_callable = self.function_value_is_callable(&loaded);
        match self.apply_untraced(loaded, args) {
            Err(Flow::Signal(sig))
                if sig.symbol_name() == "invalid-function" && !function_is_callable =>
            {
                Err(signal("invalid-function", vec![Value::from_sym_id(sym_id)]))
            }
            other => other,
        }
    }

    fn apply_evaluator_callable(
        &mut self,
        name: &str,
        args: Vec<Value>,
        wrong_arity_callee: Value,
    ) -> EvalResult {
        match name {
            "throw" => {
                if args.len() != 2 {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![wrong_arity_callee, Value::fixnum(args.len() as i64)],
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
        if sym_id == throw_symbol() {
            if args.len() != 2 {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::subr_from_sym_id(sym_id), Value::fixnum(args.len() as i64)],
                ));
            }
            let tag = args[0];
            let value = args[1];
            if self.has_active_catch(&tag) {
                Err(Flow::Throw { tag, value })
            } else {
                Err(signal("no-catch", vec![tag, value]))
            }
        } else {
            Err(signal("void-function", vec![Value::from_sym_id(sym_id)]))
        }
    }

    fn apply_lambda(&mut self, func_value: Value, args: Vec<Value>) -> EvalResult {
        let Some(params) = func_value.closure_params() else {
            return Err(signal("invalid-function", vec![func_value]));
        };
        let Some(body) = func_value.closure_body_value() else {
            return Err(signal("invalid-function", vec![func_value]));
        };
        let env = func_value.closure_env().unwrap_or(None);

        // Root the function value on the specpdl so GC can trace it
        // (keeping body, env, and params alive through the call).
        let root_count = self.specpdl.len();
        self.specpdl.push(SpecBinding::GcRoot { value: func_value });

        let call_state = match self.begin_lambda_call(params, env, &args) {
            Ok(state) => state,
            Err(err) => {
                self.unbind_to(root_count);
                return Err(err);
            }
        };
        let result = self.eval_lambda_body_value(body);
        self.finish_lambda_call(call_state);
        self.unbind_to(root_count);
        result
    }

    #[inline]
    fn bind_lexical_value_rooted(&mut self, sym: SymId, value: Value) {
        bind_lexical_value_rooted_in_specpdl(&mut self.lexenv, &mut self.specpdl, sym, value);
    }

    // -----------------------------------------------------------------------
    // Macro expansion
    // -----------------------------------------------------------------------

    pub(crate) fn with_macro_expansion_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Flow>,
    ) -> Result<T, Flow> {
        self.macro_expansion_scope_depth += 1;
        let scope_enter_start = self.macro_perf_enabled.then(std::time::Instant::now);
        let state = begin_macro_expansion_scope_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.buffers,
            &self.custom,
            self.lexenv,
        );
        if let Some(start) = scope_enter_start {
            self.macro_perf_stats
                .scope_enter
                .note_duration(start.elapsed());
        }
        let result = f(self);
        let scope_exit_start = self.macro_perf_enabled.then(std::time::Instant::now);
        finish_macro_expansion_scope_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.buffers,
            &self.custom,
            state,
        );
        if let Some(start) = scope_exit_start {
            self.macro_perf_stats
                .scope_exit
                .note_duration(start.elapsed());
        }
        self.macro_expansion_scope_depth = self.macro_expansion_scope_depth.saturating_sub(1);
        result
    }

    #[inline]
    pub(crate) fn macro_expansion_mutation_epoch(&self) -> u64 {
        self.macro_expansion_mutation_epoch
    }

    #[inline]
    pub(crate) fn note_macro_expansion_mutation(&mut self) {
        if self.macro_expansion_scope_depth > 0 {
            self.macro_expansion_mutation_epoch =
                self.macro_expansion_mutation_epoch.wrapping_add(1);
        }
    }

    fn macro_expansion_context_key(&self) -> u64 {
        self.macro_expansion_context_key_for_environment(None)
    }

    fn macro_expansion_context_key_for_environment(&self, environment: Option<Value>) -> u64 {
        fn value_identity_key(value: Value) -> u64 {
            match value.kind() {
                ValueKind::Nil => 0,
                ValueKind::T => 1,
                ValueKind::Fixnum(n) => ((n as u64).wrapping_mul(0x9E37_79B1)) ^ 0x10,
                ValueKind::Symbol(sym) => ((sym.0 as u64) << 8) ^ 0x20,
                ValueKind::Subr(sym) => {
                    ((sym.0 as u64) << 8) ^ 0x22
                }
                ValueKind::Veclike(VecLikeType::Subr) => {
                    let sym = value.as_subr_id().unwrap();
                    ((sym.0 as u64) << 8) ^ 0x22
                }
                _ => (value.bits() as u64) ^ 0x30,
            }
        }

        fn semantic_fingerprint(value: Value) -> u64 {
            use std::hash::Hasher;

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            let mut seen = std::collections::HashSet::new();
            value_fingerprint(value, &mut hasher, 4, &mut seen);
            hasher.finish()
        }

        let current_macroexpand_env = self
            .obarray()
            .symbol_value_id(macroexpand_all_environment_symbol())
            .copied()
            .unwrap_or(Value::NIL);
        let current_dynvars = self
            .obarray()
            .symbol_value_id(macroexp_dynvars_symbol())
            .copied()
            .unwrap_or(Value::NIL);

        let explicit_environment_key = environment.map(semantic_fingerprint).unwrap_or(0);
        let current_macroexpand_env_key = value_identity_key(current_macroexpand_env);
        let current_dynvars_key = value_identity_key(current_dynvars);

        explicit_environment_key.rotate_left(7)
            ^ current_macroexpand_env_key.rotate_left(29)
            ^ current_dynvars_key.rotate_left(43)
    }

    fn runtime_macro_expansion_cache_enabled(&self) -> bool {
        !self.macro_cache_disabled
            && self
                .visible_variable_value_or_nil_by_id(load_in_progress_symbol())
                .is_truthy()
    }

    fn runtime_macro_expansion_cache_key(
        &self,
        function: Value,
        args_fingerprint: u64,
        context_key: u64,
    ) -> (usize, usize, u64) {
        (
            function.bits() ^ 0x9E37_79B1usize,
            args_fingerprint as usize,
            context_key,
        )
    }

    pub(crate) fn lookup_runtime_macro_expansion(
        &mut self,
        function: Value,
        args: &[Value],
        environment: Option<Value>,
    ) -> Option<Value> {
        let perf_start = self.macro_perf_enabled.then(std::time::Instant::now);
        if !self.runtime_macro_expansion_cache_enabled() {
            if let Some(start) = perf_start {
                self.macro_perf_stats
                    .cache_lookup
                    .note_duration(start.elapsed());
            }
            return None;
        }
        let current_fp = runtime_tail_fingerprint(args);
        let context_key = self.macro_expansion_context_key_for_environment(environment);
        let cache_key = self.runtime_macro_expansion_cache_key(function, current_fp, context_key);
        let cached = self
            .runtime_macro_expansion_cache
            .get(&cache_key)
            .cloned()?;
        if cached.fingerprint == current_fp {
            self.macro_cache_hits += 1;
            if let Some(start) = perf_start {
                self.macro_perf_stats
                    .cache_lookup
                    .note_duration(start.elapsed());
            }
            return Some(cached.expanded);
        }
        if let Some(start) = perf_start {
            self.macro_perf_stats
                .cache_lookup
                .note_duration(start.elapsed());
        }
        None
    }

    pub(crate) fn store_runtime_macro_expansion(
        &mut self,
        form: Value,
        function: Value,
        args: &[Value],
        expanded_value: &Value,
        expand_elapsed: std::time::Duration,
        environment: Option<Value>,
    ) {
        let perf_start = self.macro_perf_enabled.then(std::time::Instant::now);
        if !self.runtime_macro_expansion_cache_enabled() {
            if let Some(start) = perf_start {
                self.macro_perf_stats
                    .cache_store
                    .note_duration(start.elapsed());
            }
            return;
        }
        self.macro_cache_misses += 1;
        self.macro_expand_total_us += expand_elapsed.as_micros() as u64;
        let current_fp = runtime_tail_fingerprint(args);
        let context_key = self.macro_expansion_context_key_for_environment(environment);
        let cache_key = self.runtime_macro_expansion_cache_key(function, current_fp, context_key);
        let cache_entry = RuntimeMacroExpansionCacheEntry::new(*expanded_value, current_fp);
        if self.macro_perf_enabled && expand_elapsed.as_millis() > 50 {
            let macro_head = if form.is_cons() {
                form.cons_car().as_symbol_name().unwrap_or("<non-symbol>")
            } else {
                "<atom>"
            };
            let form_str = crate::emacs_core::print::print_value(&form);
            let form_preview: String = form_str.chars().take(200).collect();
            tracing::warn!(
                "runtime_macro_cache MISS head={macro_head} macro={:#x} fp={:#x} took {expand_elapsed:.2?} form={form_preview}",
                function.bits(),
                current_fp
            );
        }
        self.runtime_macro_expansion_cache
            .insert(cache_key, cache_entry);
        if let Some(start) = perf_start {
            self.macro_perf_stats
                .cache_store
                .note_duration(start.elapsed());
        }
    }

    fn apply_macro_callable_with_dynamic_scope(
        &mut self,
        callable: Value,
        args: Vec<Value>,
    ) -> Result<Value, Flow> {
        let perf_start = self.macro_perf_enabled.then(std::time::Instant::now);
        // GNU macroexpansion runs the expander through the normal apply/call
        // path with live call-frame state holding the argument list. Mirror
        // that here so macroexpander args stay rooted in an active call frame
        // instead of an explicit eval-root adapter vector.
        let result = self.with_macro_expansion_scope(|eval| eval.apply(callable, args));
        if let Some(start) = perf_start {
            self.macro_perf_stats
                .macro_apply
                .note_duration(start.elapsed());
        }
        result
    }

    pub(crate) fn expand_macro_for_macroexpand(
        &mut self,
        form: Value,
        definition: Value,
        args: Vec<Value>,
        environment: Option<Value>,
    ) -> Result<Value, Flow> {
        let perf_start = self.macro_perf_enabled.then(std::time::Instant::now);
        if let Some(cached) = self.lookup_runtime_macro_expansion(definition, &args, environment) {
            if let Some(start) = perf_start {
                self.macro_perf_stats
                    .expand_macro
                    .note_duration(start.elapsed());
            }
            return Ok(cached);
        }
        let args_for_cache = args.clone();
        let expand_start = std::time::Instant::now();
        let specpdl_root_scope = self.save_specpdl_roots();
        self.push_specpdl_root(form);
        self.push_specpdl_root(definition);
        if let Some(environment) = environment {
            self.push_specpdl_root(environment);
        }

        let result = (|| {
            let expanded = if definition.is_macro() {
                self.apply_macro_callable_with_dynamic_scope(definition, args)?
            } else if cons_head_symbol_id(&definition) == Some(macro_symbol()) {
                self.apply_macro_callable_with_dynamic_scope(definition.cons_cdr(), args)?
            } else if self.function_value_is_callable(&definition) {
                // GNU `macroexpand` ENVIRONMENT entries store the macro
                // expander itself, not the full `(macro . fn)` function cell.
                self.apply_macro_callable_with_dynamic_scope(definition, args)?
            } else {
                return Err(signal("invalid-function", vec![definition]));
            };

            let expand_elapsed = expand_start.elapsed();
            self.store_runtime_macro_expansion(
                form,
                definition,
                &args_for_cache,
                &expanded,
                expand_elapsed,
                environment,
            );
            Ok(expanded)
        })();
        self.restore_specpdl_roots(specpdl_root_scope);
        if let Some(start) = perf_start {
            self.macro_perf_stats
                .expand_macro
                .note_duration(start.elapsed());
        }
        result
    }

    pub(crate) fn note_eager_macro_perf_step1(&mut self, duration: std::time::Duration) {
        if self.macro_perf_enabled {
            self.macro_perf_stats.eager_step1.note_duration(duration);
        }
    }

    pub(crate) fn note_eager_macro_perf_step3(&mut self, duration: std::time::Duration) {
        if self.macro_perf_enabled {
            self.macro_perf_stats.eager_step3.note_duration(duration);
        }
    }

    pub(crate) fn note_eager_macro_perf_step4(&mut self, duration: std::time::Duration) {
        if self.macro_perf_enabled {
            self.macro_perf_stats.eager_step4.note_duration(duration);
        }
    }

    pub(crate) fn macro_perf_summary(&self) -> Option<String> {
        if !self.macro_perf_enabled {
            return None;
        }

        let mut parts = vec![format!(
            "cache=hits:{} misses:{} expand-total:{:.2}ms",
            self.macro_cache_hits,
            self.macro_cache_misses,
            self.macro_expand_total_us as f64 / 1000.0
        )];

        for counter in [
            self.macro_perf_stats.scope_enter.summary("scope-enter"),
            self.macro_perf_stats.scope_exit.summary("scope-exit"),
            self.macro_perf_stats.macro_apply.summary("macro-apply"),
            self.macro_perf_stats.cache_lookup.summary("cache-lookup"),
            self.macro_perf_stats.cache_store.summary("cache-store"),
            self.macro_perf_stats.expand_macro.summary("expand-macro"),
            self.macro_perf_stats.eager_step1.summary("eager-step1"),
            self.macro_perf_stats.eager_step3.summary("eager-step3"),
            self.macro_perf_stats.eager_step4.summary("eager-step4"),
        ]
        .into_iter()
        .flatten()
        {
            parts.push(counter);
        }

        Some(parts.join(" | "))
    }

    #[inline]
    pub(crate) fn macro_perf_enabled(&self) -> bool {
        self.macro_perf_enabled
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

        // Phase 10D: handle FORWARDED BUFFER_OBJFWD specbind separately
        // from the legacy LOCALIZED path. Mirrors GNU `specbind`
        // SYMBOL_FORWARDED arm at `eval.c:3641-3677`.
        {
            use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
            use crate::emacs_core::symbol::SymbolRedirect;
            let forwarded = self
                .obarray
                .get_by_id(resolved)
                .filter(|s| s.redirect() == SymbolRedirect::Forwarded)
                .map(|s| unsafe { s.val.fwd });
            if let Some(fwd_ptr) = forwarded {
                let fwd = unsafe { &*fwd_ptr };
                if matches!(fwd.ty, LispFwdType::BufferObj) {
                    let buf_fwd = unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                    let off = buf_fwd.offset as usize;
                    let flags_idx = buf_fwd.local_flags_idx;
                    let buf_id_opt = self.buffers.current_buffer_id();
                    let has_local = match buf_id_opt {
                        Some(id) => self
                            .buffers
                            .get(id)
                            .map(|buf| flags_idx < 0 || buf.slot_local_flag(off))
                            .unwrap_or(false),
                        None => false,
                    };
                    if has_local {
                        // SPECPDL_LET_LOCAL — save the current
                        // per-buffer slot value, then overwrite. On
                        // unbind we restore via set_buffer_local
                        // which writes back to the slot.
                        let buf_id = buf_id_opt.expect("has_local implies current buffer");
                        let old_val = self
                            .buffers
                            .get(buf_id)
                            .map(|b| b.slots[off])
                            .unwrap_or(Value::NIL);
                        self.specpdl.push(SpecBinding::LetLocal {
                            sym_id: resolved,
                            old_value: old_val,
                            buffer_id: buf_id,
                        });
                        if self.watchers.has_watchers(resolved) {
                            let _ = self.run_variable_watchers_by_id(
                                resolved,
                                &value,
                                &Value::NIL,
                                "let",
                            );
                        }
                        if let Some(buf) = self.buffers.get_mut(buf_id) {
                            buf.slots[off] = value;
                            // Always-local slots need no flag
                            // change; conditional slots already
                            // have the bit set (has_local check).
                        }
                        return;
                    } else {
                        // SPECPDL_LET_DEFAULT — save old default,
                        // propagate the new value via
                        // set_buffer_default_slot. On unbind we
                        // propagate the saved default back.
                        let old_default = if off < self.buffers.buffer_defaults.len() {
                            Some(self.buffers.buffer_defaults[off])
                        } else {
                            Some(buf_fwd.default)
                        };
                        self.specpdl.push(SpecBinding::LetDefault {
                            sym_id: resolved,
                            old_value: old_default,
                        });
                        if self.watchers.has_watchers(resolved) {
                            let _ = self.run_variable_watchers_by_id(
                                resolved,
                                &value,
                                &Value::NIL,
                                "let",
                            );
                        }
                        let info_ref =
                            crate::buffer::buffer::lookup_buffer_slot_by_sym_id(resolved);
                        if let Some(info) = info_ref {
                            self.buffers.set_buffer_default_slot(info, value);
                        }
                        return;
                    }
                }
            }
        }

        // Phase 10E: SYMBOL_LOCALIZED specbind. Mirrors GNU `specbind`
        // SYMBOL_LOCALIZED arm at `eval.c:3641-3677`:
        //
        //   1. Read the current value (forces BLV swap-in to current
        //      buffer).
        //   2. Tentatively record SPECPDL_LET_LOCAL with the captured
        //      value and buffer.
        //   3. If !blv_found(blv) (the swap-in landed on defcell, not
        //      a per-buffer alist entry), demote to SPECPDL_LET_DEFAULT.
        //   4. Call set_internal_localized(BIND) to write the new
        //      value into wherever the BLV cache currently points.
        if let Some(sym_slot) = self.obarray.get_by_id(resolved)
            && sym_slot.redirect() == crate::emacs_core::symbol::SymbolRedirect::Localized
        {
            if let Some(buf_id) = self.buffers.current_buffer_id() {
                let (cur_val, alist) = match self.buffers.get(buf_id) {
                    Some(buf) => (Value::make_buffer(buf.id), buf.local_var_alist),
                    None => (Value::NIL, Value::NIL),
                };
                // Force a swap so blv.found / blv.valcell match the
                // current buffer state. After this, blv.where_buf =
                // cur_val.
                let old_val = self
                    .obarray
                    .find_symbol_value_in_buffer(
                        resolved,
                        Some(buf_id),
                        cur_val,
                        alist,
                        None,
                        0u64,
                        None,
                    )
                    .unwrap_or(Value::NIL);
                let blv_found = self.obarray.blv(resolved).map(|b| b.found).unwrap_or(false);
                if blv_found {
                    self.specpdl.push(SpecBinding::LetLocal {
                        sym_id: resolved,
                        old_value: old_val,
                        buffer_id: buf_id,
                    });
                } else {
                    self.specpdl.push(SpecBinding::LetDefault {
                        sym_id: resolved,
                        old_value: Some(old_val),
                    });
                }
                if self.watchers.has_watchers(resolved) {
                    let _ = self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "let");
                }
                // Write the new value via set_internal_localized
                // with bindflag=Bind. Bind never auto-creates a new
                // alist entry, so a let on a non-buffer-local
                // LOCALIZED symbol writes to defcell.cdr (the
                // global default), matching GNU.
                let new_alist = self.obarray.set_internal_localized(
                    resolved,
                    value,
                    cur_val,
                    alist,
                    crate::emacs_core::symbol::SetInternalBind::Bind,
                    false,
                );
                if let Some(buf) = self.buffers.get_mut(buf_id) {
                    buf.local_var_alist = new_alist;
                }
                self.sync_cached_runtime_binding_by_id(resolved, value);
                return;
            }
        }

        // Plain value path (GNU: SYMBOL_PLAINVAL)
        let old_value = self.obarray.symbol_value_id(resolved).copied();
        self.specpdl.push(SpecBinding::Let {
            sym_id: resolved,
            old_value,
        });
        if self.watchers.has_watchers(resolved) {
            let _ = self.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "let");
        }
        self.obarray.set_symbol_value_id(resolved, value);
        self.sync_cached_runtime_binding_by_id(resolved, value);
    }

    /// Check if a `let` is currently shadowing a buffer-local
    /// variable's binding. Matches GNU
    /// `eval.c:3559-3577 (let_shadows_buffer_binding_p)`.
    ///
    /// When true, `setq` inside the let should modify the existing
    /// binding (whichever specpdl record is on top) rather than
    /// auto-creating a brand-new per-buffer binding.
    ///
    /// GNU walks the specpdl looking for either SPECPDL_LET_LOCAL
    /// or SPECPDL_LET_DEFAULT records keyed to the symbol; both
    /// trigger the shadow behavior. neomacs's Phase 7 stub used to
    /// only check `LetDefault`, missing the LetLocal arm. Buffer-
    /// local audit Medium 4 in
    /// `drafts/buffer-local-variables-audit.md`.
    pub(crate) fn let_shadows_buffer_binding_p(&self, sym_id: SymId) -> bool {
        self.specpdl.iter().rev().any(|entry| match entry {
            SpecBinding::LetDefault { sym_id: s, .. } => *s == sym_id,
            SpecBinding::LetLocal { sym_id: s, .. } => *s == sym_id,
            SpecBinding::Let { .. }
            | SpecBinding::LexicalEnv { .. }
            | SpecBinding::GcRoot { .. }
            | SpecBinding::Backtrace { .. }
            | SpecBinding::Nop
            | SpecBinding::UnwindProtect { .. }
            | SpecBinding::SaveExcursion { .. }
            | SpecBinding::SaveCurrentBuffer { .. }
            | SpecBinding::SaveRestriction { .. } => false,
        })
    }

    /// Restore all specpdl bindings back to `count`.
    /// Matches GNU Emacs's unbind_to() in eval.c.
    pub(crate) fn unbind_to(&mut self, count: usize) {
        // Mirrors GNU `unbind_to` in `eval.c:3907-3930`: suppress a
        // pending quit during cleanup so `unwind-protect` cleanup forms
        // run to completion, then restore the pending state on exit if
        // no inner form replaced it. Without this an interactive `C-g`
        // arriving during a long-running protected form would abort the
        // CLEANUP clause mid-way, leaving resources in a bad state.
        let quitf = self.quit_flag_value();
        if !quitf.is_nil() {
            self.set_quit_flag_value(Value::NIL);
        }
        while self.specpdl.len() > count {
            let binding = self.specpdl.pop().unwrap();
            match binding {
                SpecBinding::Let { sym_id, old_value } => {
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(sym_id) {
                        let restore_val = old_value.unwrap_or(Value::NIL);
                        let _ = self.run_variable_watchers_by_id(
                            sym_id,
                            &restore_val,
                            &Value::NIL,
                            "unlet",
                        );
                    }
                    match old_value {
                        Some(val) => {
                            self.obarray.set_symbol_value_id(sym_id, val);
                            self.sync_cached_runtime_binding_by_id(sym_id, val);
                        }
                        None => {
                            self.obarray.makunbound_id(sym_id);
                            self.sync_cached_runtime_binding_by_id(sym_id, Value::NIL);
                        }
                    }
                }
                SpecBinding::LetLocal {
                    sym_id,
                    old_value,
                    buffer_id,
                } => {
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(sym_id) {
                        let _ = self.run_variable_watchers_by_id(
                            sym_id,
                            &old_value,
                            &Value::NIL,
                            "unlet",
                        );
                    }
                    // Restore only if the buffer is still live.
                    // Mirrors GNU `do_one_unbind` SPECPDL_LET_LOCAL
                    // arm at `eval.c:3838-3850`:
                    //     if (!NILP (Flocal_variable_p (symbol, where)))
                    //       set_internal (symbol, old_value, where, UNBIND);
                    if self.buffers.get(buffer_id).is_some() {
                        // Phase 10E: for LOCALIZED symbols, restore via
                        // set_internal_localized(UNBIND) targeting the
                        // saved buffer. This walks the buffer's alist
                        // and rewrites the cell's cdr in place,
                        // matching GNU's set_internal LOCALIZED arm
                        // and bypassing the legacy lisp_bindings path.
                        use crate::emacs_core::symbol::{SetInternalBind, SymbolRedirect};
                        let is_localized = self
                            .obarray
                            .get_by_id(sym_id)
                            .map(|s| s.redirect() == SymbolRedirect::Localized)
                            .unwrap_or(false);
                        if is_localized {
                            let buf_val = Value::make_buffer(buffer_id);
                            let alist = self
                                .buffers
                                .get(buffer_id)
                                .map(|b| b.local_var_alist)
                                .unwrap_or(Value::NIL);
                            let new_alist = self.obarray.set_internal_localized(
                                sym_id,
                                old_value,
                                buf_val,
                                alist,
                                SetInternalBind::Unbind,
                                false,
                            );
                            if let Some(buf) = self.buffers.get_mut(buffer_id) {
                                buf.local_var_alist = new_alist;
                            }
                        } else {
                            let _ = self
                                .buffers
                                .set_buffer_local_property_by_sym_id(buffer_id, sym_id, old_value);
                        }
                        self.sync_cached_runtime_binding_by_id(sym_id, old_value);
                    }
                }
                SpecBinding::LetDefault { sym_id, old_value } => {
                    // Restore the default value (GNU: set_default_internal)
                    let name = resolve_sym(sym_id);
                    if self.watchers.has_watchers(sym_id) {
                        let restore_val = old_value.unwrap_or(Value::NIL);
                        let _ = self.run_variable_watchers_by_id(
                            sym_id,
                            &restore_val,
                            &Value::NIL,
                            "unlet",
                        );
                    }
                    // Phase 10D: FORWARDED BUFFER_OBJFWD restores
                    // through `set_buffer_default_slot` so the
                    // change propagates to every non-local buffer's
                    // slot, mirroring GNU `set_default_internal`
                    // SYMBOL_FORWARDED arm.
                    use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
                    use crate::emacs_core::symbol::SymbolRedirect;
                    let forwarded_slot = self
                        .obarray
                        .get_by_id(sym_id)
                        .filter(|s| s.redirect() == SymbolRedirect::Forwarded)
                        .and_then(|s| {
                            let fwd = unsafe { &*s.val.fwd };
                            if matches!(fwd.ty, LispFwdType::BufferObj) {
                                let buf_fwd =
                                    unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                                crate::buffer::buffer::lookup_buffer_slot_by_sym_id(sym_id)
                                    .map(|info| (info, buf_fwd))
                            } else {
                                None
                            }
                        });
                    if let Some((info, _buf_fwd)) = forwarded_slot {
                        if let Some(val) = old_value {
                            self.buffers.set_buffer_default_slot(info, val);
                        }
                        continue;
                    }
                    match old_value {
                        Some(val) => {
                            self.obarray.set_symbol_value_id(sym_id, val);
                            self.sync_cached_runtime_binding_by_id(sym_id, val);
                        }
                        None => {
                            self.obarray.makunbound_id(sym_id);
                            self.sync_cached_runtime_binding_by_id(sym_id, Value::NIL);
                        }
                    }
                }
                SpecBinding::LexicalEnv { old_lexenv } => {
                    // Mirrors GNU unbind_to for
                    // specbind(Qinternal_interpreter_environment, ...).
                    self.lexenv = old_lexenv;
                }
                SpecBinding::GcRoot { .. } => {}
                SpecBinding::Backtrace { .. } => {
                    // No-op, matches GNU SPECPDL_BACKTRACE
                }
                SpecBinding::Nop => {
                    // No-op, matches GNU SPECPDL_NOP
                }
                SpecBinding::UnwindProtect { forms: cleanup, lexenv } => {
                    // Entry already popped — re-entrant errors won't re-unwind.
                    let saved_lexenv = self.lexenv;
                    self.lexenv = lexenv;
                    if cleanup.is_cons() || cleanup.is_nil() {
                        // Interpreter path: list of forms
                        let _ = self.sf_progn_value(cleanup);
                    } else {
                        // VM path: callable (bytecode function)
                        let _ = self.apply(cleanup, vec![]);
                    }
                    self.lexenv = saved_lexenv;
                }
                SpecBinding::SaveExcursion { buffer_id, marker_id } => {
                    self.restore_current_buffer_if_live(buffer_id);
                    if let Some(saved_pt) = self.buffers.marker_position(buffer_id, marker_id) {
                        let _ = self.buffers.goto_buffer_byte(buffer_id, saved_pt);
                    }
                    self.buffers.remove_marker(marker_id);
                }
                SpecBinding::SaveCurrentBuffer { buffer_id } => {
                    self.restore_current_buffer_if_live(buffer_id);
                }
                SpecBinding::SaveRestriction { state } => {
                    self.buffers.restore_saved_restriction_state(state);
                }
            }
        }
        // If cleanup forms didn't set their own quit, reinstate the
        // pending state. Matches `eval.c:3927-3928`.
        if self.quit_flag_value().is_nil() && !quitf.is_nil() {
            self.set_quit_flag_value(quitf);
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
            SpecBinding::LexicalEnv { .. } => {
                // Standalone path doesn't have self.lexenv access.
                // This variant should not appear on the standalone
                // specpdl (used by bytecode VM which has its own
                // LexicalEnv / specbind mechanism).
                tracing::warn!("unbind_to_in_state: LexicalEnv without Context");
            }
            SpecBinding::GcRoot { .. } => {}
            SpecBinding::Backtrace { .. }
            | SpecBinding::Nop
            | SpecBinding::UnwindProtect { .. }
            | SpecBinding::SaveExcursion { .. }
            | SpecBinding::SaveCurrentBuffer { .. }
            | SpecBinding::SaveRestriction { .. } => {
                // These should not appear in standalone unbind_to_in_state.
                // Once the VM is fully migrated, this function may be removed.
            }
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
        SpecBinding::LetLocal { .. }
        | SpecBinding::LexicalEnv { .. }
        | SpecBinding::GcRoot { .. }
        | SpecBinding::Backtrace { .. }
        | SpecBinding::Nop
        | SpecBinding::UnwindProtect { .. }
        | SpecBinding::SaveExcursion { .. }
        | SpecBinding::SaveCurrentBuffer { .. }
        | SpecBinding::SaveRestriction { .. } => false,
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
        Some(SpecBinding::LetLocal { .. })
        | Some(SpecBinding::LexicalEnv { .. })
        | Some(SpecBinding::GcRoot { .. })
        | Some(SpecBinding::Backtrace { .. })
        | Some(SpecBinding::Nop)
        | Some(SpecBinding::UnwindProtect { .. })
        | Some(SpecBinding::SaveExcursion { .. })
        | Some(SpecBinding::SaveCurrentBuffer { .. })
        | Some(SpecBinding::SaveRestriction { .. }) => {
            unreachable!("non-variable bindings are excluded above")
        }
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
            | SpecBinding::LetLocal { .. }
            | SpecBinding::LexicalEnv { .. }
            | SpecBinding::GcRoot { .. }
            | SpecBinding::Backtrace { .. }
            | SpecBinding::Nop
            | SpecBinding::UnwindProtect { .. }
            | SpecBinding::SaveExcursion { .. }
            | SpecBinding::SaveCurrentBuffer { .. }
            | SpecBinding::SaveRestriction { .. } => {}
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
    _custom: &CustomManager,
    specpdl: &[SpecBinding],
    sym_id: SymId,
    value: Value,
) -> Option<crate::buffer::BufferId> {
    use crate::emacs_core::symbol::{SetInternalBind, SymbolRedirect};

    let symbol_is_canonical = super::builtins::is_canonical_symbol_id(sym_id);

    // Phase 10E: route writes for LOCALIZED symbols through the BLV
    // machinery. Mirrors GNU `set_internal` SYMBOL_LOCALIZED arm
    // (`data.c:1687-1762`) and the vm.rs assign_var_id LOCALIZED
    // path — keeps the eval.rs and vm.rs hot paths semantically
    // identical so a buffer-local visible from the bytecode VM is
    // also visible from the tree-walk interpreter and the `set`
    // builtin.
    let redirect = obarray.get_by_id(sym_id).map(|s| s.redirect());
    if symbol_is_canonical
        && matches!(redirect, Some(SymbolRedirect::Localized))
        && let Some(buf_id) = buffers.current_buffer_id()
    {
        let (cur_val, alist) = match buffers.get(buf_id) {
            Some(buf) => (Value::make_buffer(buf.id), buf.local_var_alist),
            None => (Value::NIL, Value::NIL),
        };
        let let_shadows = specpdl.iter().rev().any(
            |entry| matches!(entry, SpecBinding::LetDefault { sym_id: s, .. } if *s == sym_id),
        );
        let new_alist = obarray.set_internal_localized(
            sym_id,
            value,
            cur_val,
            alist,
            SetInternalBind::Set,
            let_shadows,
        );
        if let Some(buf) = buffers.get_mut(buf_id) {
            buf.local_var_alist = new_alist;
        }
        return Some(buf_id);
    }

    // If the buffer already has a local binding (slot-backed), write
    // to it. Mirrors GNU `set_internal` SYMBOL_FORWARDED arm for
    // BUFFER_OBJFWD slots.
    if symbol_is_canonical
        && let Some(current_id) = buffers.current_buffer_id()
        && let Some(buf) = buffers.get(current_id)
    {
        if buf.has_buffer_local_by_sym_id(sym_id) {
            let _ = buffers.set_buffer_local_property_by_sym_id(current_id, sym_id, value);
            return Some(current_id);
        }
    }

    obarray.set_symbol_value_id(sym_id, value);
    None
}

pub(crate) fn makunbound_runtime_binding_in_state(
    obarray: &mut Obarray,
    buffers: &mut BufferManager,
    _custom: &CustomManager,
    _specpdl: &[SpecBinding],
    sym_id: SymId,
) {
    let symbol_is_canonical = super::builtins::is_canonical_symbol_id(sym_id);

    // specbind writes directly to obarray, so no dynamic frame lookup needed.

    if symbol_is_canonical
        && let Some(current_id) = buffers.current_buffer_id()
        && let Some(buf) = buffers.get(current_id)
        && buf.has_buffer_local_by_sym_id(sym_id)
    {
        let _ = buffers.set_buffer_local_void_property_by_sym_id(current_id, sym_id);
        return;
    }

    // Mirrors GNU `set_internal` SYMBOL_LOCALIZED arm with
    // `unbinding_p = true` (`src/data.c:1687-1762`). The BLV's
    // `local_if_set` flag determines whether to create a per-buffer
    // void binding; LOCALIZED symbols carry a BLV so this fires only
    // for them.
    let local_if_set = obarray
        .blv(sym_id)
        .map(|blv| blv.local_if_set)
        .unwrap_or(false);
    if symbol_is_canonical && local_if_set {
        if let Some(current_id) = buffers.current_buffer_id() {
            let _ = buffers.set_buffer_local_void_property_by_sym_id(current_id, sym_id);
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
            let name_id = symbol_name_id(sym_id);
            let (min_args, max_args, dispatch_kind) =
                super::subr_info::lookup_compat_subr_metadata(name, 0, None);
            // Register in global static table so lookups by sym_id work
            register_global_subr_entry(sym_id, SubrEntry {
                function: None, // evaluator-handled, no SubrFn
                min_args,
                max_args,
                dispatch_kind,
                name_id,
            });
            self.obarray.intern(name);
            self.obarray
                .set_symbol_function_id(sym_id, Value::subr_from_sym_id(sym_id));
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
        self.defsubr_with_entry(
            name,
            crate::tagged::header::SubrFn::Many(func),
            min_args,
            max_args,
        );
    }

    pub fn defsubr_0(&mut self, name: &str, func: fn(&mut Context) -> EvalResult) {
        self.defsubr_with_entry(name, crate::tagged::header::SubrFn::A0(func), 0, Some(0));
    }

    pub fn defsubr_1(
        &mut self,
        name: &str,
        func: fn(&mut Context, Value) -> EvalResult,
        min_args: u16,
    ) {
        self.defsubr_with_entry(
            name,
            crate::tagged::header::SubrFn::A1(func),
            min_args,
            Some(1),
        );
    }

    pub fn defsubr_2(
        &mut self,
        name: &str,
        func: fn(&mut Context, Value, Value) -> EvalResult,
        min_args: u16,
    ) {
        self.defsubr_with_entry(
            name,
            crate::tagged::header::SubrFn::A2(func),
            min_args,
            Some(2),
        );
    }

    fn defsubr_with_entry(
        &mut self,
        name: &str,
        func: crate::tagged::header::SubrFn,
        min_args: u16,
        max_args: Option<u16>,
    ) {
        let (min_args, max_args, dispatch_kind) =
            super::subr_info::lookup_compat_subr_metadata(name, min_args, max_args);
        let sym_id = intern(name);
        let name_id = symbol_name_id(sym_id);

        // Register in global static table
        register_global_subr_entry(sym_id, SubrEntry {
            function: Some(func),
            min_args,
            max_args,
            dispatch_kind,
            name_id,
        });

        // Set symbol function cell to new immediate subr value
        self.obarray.intern(name);
        self.obarray.set_symbol_function(name, Value::subr_from_sym_id(sym_id));
    }

    /// Call a registered subr value directly. Returns None if VALUE is not a
    /// fully registered subr.
    pub fn dispatch_subr_value(&mut self, function: Value, args: Vec<Value>) -> Option<EvalResult> {
        let sym_id = function.as_subr_id()?;
        let wrong_arity_callee = Value::symbol(resolve_sym(sym_id));
        self.dispatch_subr_value_internal(function, args, wrong_arity_callee)
    }

    /// Resolve a symbol identity to its canonical subr object and call it.
    /// Returns None if the symbol's canonical name has no registered subr.
    /// Supports uninterned symbols: falls back to canonical SymId via NameId lookup.
    pub fn dispatch_subr_id(&mut self, sym_id: SymId, args: Vec<Value>) -> Option<EvalResult> {
        // Try the sym_id directly first
        let resolved = if lookup_global_subr_entry(sym_id).is_some() {
            sym_id
        } else {
            // Fall back to canonical symbol for this name (handles uninterned SymIds)
            let name_id = symbol_name_id(sym_id);
            let canonical = crate::emacs_core::intern::canonical_symbol_for_name(name_id)?;
            lookup_global_subr_entry(canonical)?;
            canonical
        };
        let function = Value::subr_from_sym_id(resolved);
        self.dispatch_subr_value(function, args)
    }

    pub fn dispatch_subr(&mut self, name: &str, args: Vec<Value>) -> Option<EvalResult> {
        self.dispatch_subr_id(intern(name), args)
    }

    // -----------------------------------------------------------------------
    // Methods previously on VmSharedState, now on Context directly
    // -----------------------------------------------------------------------

    pub(crate) fn begin_eval_with_lexical_arg(
        &mut self,
        lexical_arg: Option<Value>,
    ) -> Result<ActiveEvalLexicalArgState, Flow> {
        begin_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.specpdl,
            lexical_arg,
        )
    }

    pub(crate) fn finish_eval_with_lexical_arg(&mut self, state: ActiveEvalLexicalArgState) {
        finish_eval_with_lexical_arg_in_state(
            &mut self.obarray,
            &mut self.lexenv,
            &mut self.specpdl,
            state,
        );
    }

    pub(crate) fn begin_macro_expansion_scope(&mut self) -> ActiveMacroExpansionScopeState {
        self.macro_expansion_scope_depth += 1;
        begin_macro_expansion_scope_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.buffers,
            &self.custom,
            self.lexenv,
        )
    }

    pub(crate) fn finish_macro_expansion_scope(&mut self, state: ActiveMacroExpansionScopeState) {
        finish_macro_expansion_scope_in_state(
            &mut self.obarray,
            &mut self.specpdl,
            &mut self.buffers,
            &self.custom,
            state,
        );
        self.macro_expansion_scope_depth = self.macro_expansion_scope_depth.saturating_sub(1);
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
        // GNU's `command_loop_level` starts at -1 before entering the
        // top-level recursive edit, so ordinary interactive execution happens
        // at level 0. Neomacs stores the raw active-loop count instead
        // (0 outside the loop, 1 at top level), so translate here to the
        // GNU-visible level used by mode-line and minibuffer semantics.
        self.command_loop.recursive_depth.saturating_sub(1)
    }

    fn sync_current_buffer_to_selected_window(&mut self) {
        let Some(frame_id) = self.frames.selected_frame().map(|frame| frame.id) else {
            return;
        };
        super::window_cmds::remember_selected_window_point_in_state(
            &mut self.frames,
            &self.buffers,
            frame_id,
        );
        super::window_cmds::sync_selected_window_buffer_in_state(
            &self.frames,
            &mut self.buffers,
            frame_id,
        );
        let _ = self.sync_current_buffer_runtime_state();
    }

    // -----------------------------------------------------------------------

    pub(crate) fn lexenv_assq_cached_in(&self, lexenv: Value, sym_id: SymId) -> Option<Value> {
        let lexenv_bits = lexenv.bits();
        let mut cache = self.lexenv_assq_cache.borrow_mut();
        if let Some(cell) = cache.find(lexenv_bits, sym_id) {
            return Some(cell);
        }

        let cell = lexenv_assq(lexenv, sym_id)?;
        cache.push(LexenvAssqCacheEntry {
            lexenv_bits,
            symbol: sym_id,
            cell,
        });
        Some(cell)
    }

    pub(crate) fn lexenv_lookup_cached_in(&self, lexenv: Value, sym_id: SymId) -> Option<Value> {
        self.lexenv_assq_cached_in(lexenv, sym_id)
            .map(|cell| cell.cons_cdr())
    }

    pub(crate) fn lexenv_declares_special_cached_in(&self, lexenv: Value, sym_id: SymId) -> bool {
        let lexenv_bits = lexenv.bits();
        let mut cache = self.lexenv_special_cache.borrow_mut();
        if let Some(declared_special) = cache.find(lexenv_bits, sym_id) {
            return declared_special;
        }

        let declared_special = lexenv_declares_special(lexenv, sym_id);
        cache.push(LexenvSpecialCacheEntry {
            lexenv_bits,
            symbol: sym_id,
            declared_special,
        });
        declared_special
    }

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
        // GNU `setq` follows the same rule as `eval_sub`: if a lexical binding
        // cell exists, mutate it directly. Declared-special affects whether
        // that cell was created, not whether assignment should reuse it.
        if self.lexical_binding() {
            if let Some(cell_id) = self.lexenv_assq_cached_in(self.lexenv, sym_id) {
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
        self.sync_cached_runtime_binding_by_id(sym_id, value);
        self.sync_keyboard_runtime_binding_by_id(sym_id, value);
        self.refresh_gc_runtime_settings_after_change_by_id(sym_id);
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
        self.sync_cached_runtime_binding_by_id(sym_id, value);
        self.sync_keyboard_runtime_binding_by_id(sym_id, value);
        self.refresh_gc_runtime_settings_after_change_by_id(sym_id);
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
        self.sync_cached_runtime_binding_by_id(sym_id, Value::NIL);
        self.sync_keyboard_runtime_binding_by_id(sym_id, Value::NIL);
        self.refresh_gc_runtime_settings_after_change_by_id(sym_id);
    }

    fn has_local_binding_by_id(&self, sym_id: SymId) -> bool {
        self.lexenv_assq_cached_in(self.lexenv, sym_id).is_some()
            || self
                .specpdl
                .iter()
                .rev()
                .any(|entry| matches!(entry, SpecBinding::Let { sym_id: s, .. } if *s == sym_id))
    }

    pub(crate) fn visible_variable_value_or_nil(&self, name: &str) -> Value {
        self.visible_variable_value_or_nil_by_id(intern(name))
    }

    pub(crate) fn visible_variable_value_or_nil_by_id(&self, sym_id: SymId) -> Value {
        if let Some(value) = self.lexenv_lookup_cached_in(self.lexenv, sym_id) {
            return value;
        }
        if let Ok(Some(value)) = self.visible_runtime_variable_value_by_id(sym_id) {
            return value;
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
        let resolved_is_canonical = is_canonical_id(resolved);

        // Phase 10E: route LOCALIZED reads through the BLV
        // machinery so they observe writes from set_internal_localized.
        // Mirrors GNU `find_symbol_value` LOCALIZED arm
        // (`data.c:1620-1650`). Without this, `symbol-value` returns
        // the stale `SymbolValue::BufferLocal::default` field for
        // LOCALIZED variables that have a per-buffer binding.
        if resolved_is_canonical && let Some(buf) = self.buffers.current_buffer() {
            use crate::emacs_core::symbol::SymbolRedirect;
            if let Some(sym) = self.obarray.get_by_id(resolved)
                && sym.redirect() == SymbolRedirect::Localized
            {
                let target_buf = Value::make_buffer(buf.id);
                if let Some(value) =
                    self.obarray
                        .read_localized(resolved, target_buf, buf.local_var_alist)
                {
                    // `Qunbound` means the LOCALIZED binding is
                    // void in this buffer — return None so the
                    // caller signals `void-variable`. Mirrors GNU
                    // `Fsymbol_value` treating `Qunbound` from
                    // `find_symbol_value` as void.
                    if value.is_unbound() {
                        return None;
                    }
                    return Some(value);
                }
            }
        }

        // Buffer-local bindings for FORWARDED BUFFER_OBJFWD slots: when
        // `make-local-variable` enables the per-buffer flag, reads must
        // return the slot value, not the default. Mirrors GNU
        // `find_symbol_value` (`data.c:1585`). Canonical symbols only.
        if resolved_is_canonical
            && let Some(buf) = self.buffers.current_buffer()
            && let Some(binding) = buf.get_buffer_local_binding_by_sym_id(resolved)
        {
            return binding.as_value();
        }

        // Phase 10D: FORWARDED BUFFER_OBJFWD reads consult
        // `BufferManager::buffer_defaults` (the live default) rather
        // than the legacy `symbol_value_id` reader, which is the
        // legacy enum dispatcher and returns None for FORWARDED.
        // Mirrors GNU `do_symval_forwarding` BUFFER_OBJFWD reading
        // through `buffer_defaults` when there's no per-buffer local.
        {
            use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
            use crate::emacs_core::symbol::SymbolRedirect;
            if let Some(sym) = self.obarray.get_by_id(resolved)
                && sym.redirect() == SymbolRedirect::Forwarded
            {
                let fwd = unsafe { &*sym.val.fwd };
                if matches!(fwd.ty, LispFwdType::BufferObj) {
                    let buf_fwd = unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                    let off = buf_fwd.offset as usize;
                    if off < self.buffers.buffer_defaults.len() {
                        return Some(self.buffers.buffer_defaults[off]);
                    }
                    return Some(buf_fwd.default);
                }
            }
        }

        if let Some(value) = self.obarray.symbol_value_id(resolved).copied() {
            return Some(value);
        }

        if resolved_is_canonical && resolved == nil_symbol() {
            return Some(Value::NIL);
        }
        if resolved_is_canonical && resolved == t_symbol() {
            return Some(Value::T);
        }
        if is_keyword_id(resolved) {
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

    pub(crate) fn run_variable_watchers_by_id(
        &mut self,
        sym_id: SymId,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_by_id_with_where(
            sym_id,
            new_value,
            old_value,
            operation,
            &Value::NIL,
        )
    }

    pub(crate) fn run_variable_watchers_by_id_with_where(
        &mut self,
        sym_id: SymId,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        if !self.watchers.has_watchers(sym_id) {
            return Ok(());
        }
        let calls =
            self.watchers
                .notify_watchers(sym_id, new_value, old_value, operation, where_value);
        for (callback, args) in calls {
            let _ = self.apply(callback, args)?;
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
        self.run_variable_watchers_by_id(intern(name), new_value, old_value, operation)
    }

    pub(crate) fn run_variable_watchers_with_where(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_by_id_with_where(
            intern(name),
            new_value,
            old_value,
            operation,
            where_value,
        )
    }

    pub(crate) fn assign_with_watchers(
        &mut self,
        name: &str,
        value: Value,
        operation: &str,
    ) -> EvalResult {
        self.assign_with_watchers_by_id(intern(name), value, operation)
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
        self.run_variable_watchers_by_id_with_where(
            sym_id,
            &value,
            &Value::NIL,
            operation,
            &where_value,
        )?;
        Ok(value)
    }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "eval_test.rs"]
mod tests;
fn runtime_string_value(value: Value) -> String {
    value
        .as_runtime_string_owned()
        .expect("ValueKind::String must carry LispString payload")
}
