//! Lisp value representation and fundamental operations.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::intern::{SymId, intern, resolve_sym};
use crate::buffer::text_props::TextPropertyTable;
use crate::gc::GcTrace;

thread_local! {
    static FLOAT_ALLOC_ID: Cell<u32> = const { Cell::new(0) };
}

/// Allocate a fresh float identity. Each call returns a unique u32
/// (within the current thread), matching GNU Emacs's `make_float`
/// semantics where every float creation produces a distinct object.
pub fn next_float_id() -> u32 {
    FLOAT_ALLOC_ID.with(|c| {
        let id = c.get();
        c.set(id.wrapping_add(1));
        id
    })
}

/// An insertion-order-preserving map from SymId to Value.
///
/// Used for lexical and dynamic environment frames where iteration order must
/// match the original binding order. This is critical for oclosure
/// compatibility: `oclosure--copy` reads the closure's env via `aref` and
/// pairs variables positionally with new arg values. A `HashMap` loses
/// insertion order and causes wrong variable-to-value bindings.
#[derive(Debug, Clone)]
pub struct OrderedSymMap {
    entries: Vec<(SymId, Value)>,
}

impl PartialEq for OrderedSymMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl OrderedSymMap {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
        }
    }

    pub fn get(&self, key: &SymId) -> Option<&Value> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: SymId, value: Value) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|(k, _)| *k == key) {
            entry.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    pub fn contains_key(&self, key: &SymId) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.entries.iter_mut().map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SymId, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reconstruct from a vec of entries (for pdump load).
    pub(crate) fn from_entries(entries: Vec<(SymId, Value)>) -> Self {
        Self { entries }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RuntimeBindingValue {
    Bound(Value),
    Void,
}

impl RuntimeBindingValue {
    pub fn bound(value: Value) -> Self {
        Self::Bound(value)
    }

    pub fn as_value(self) -> Option<Value> {
        match self {
            Self::Bound(value) => Some(value),
            Self::Void => None,
        }
    }

    pub fn as_ref(&self) -> Option<&Value> {
        match self {
            Self::Bound(value) => Some(value),
            Self::Void => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderedRuntimeBindingMap {
    entries: Vec<(SymId, RuntimeBindingValue)>,
}

impl PartialEq for OrderedRuntimeBindingMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl OrderedRuntimeBindingMap {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
        }
    }

    pub fn get(&self, key: &SymId) -> Option<&Value> {
        self.get_binding(key).and_then(RuntimeBindingValue::as_ref)
    }

    pub fn get_binding(&self, key: &SymId) -> Option<&RuntimeBindingValue> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: SymId, value: Value) {
        self.insert_binding(key, RuntimeBindingValue::Bound(value));
    }

    pub fn insert_binding(&mut self, key: SymId, value: RuntimeBindingValue) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|(k, _)| *k == key) {
            entry.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    pub fn set_void(&mut self, key: SymId) {
        self.insert_binding(key, RuntimeBindingValue::Void);
    }

    pub fn contains_key(&self, key: &SymId) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().filter_map(|(_, v)| v.as_ref())
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.entries.iter_mut().filter_map(|(_, v)| match v {
            RuntimeBindingValue::Bound(value) => Some(value),
            RuntimeBindingValue::Void => None,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SymId, &RuntimeBindingValue)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn from_entries(entries: Vec<(SymId, RuntimeBindingValue)>) -> Self {
        Self { entries }
    }
}

use crate::gc::heap::LispHeap;
use crate::gc::types::ObjId;

const ZERO_COUNT: u64 = 0;

static CONS_CELLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static FLOATS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static VECTOR_CELLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static SYMBOLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static STRING_CHARS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static INTERVALS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static STRINGS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
thread_local! {
    static STRING_TEXT_PROPS: RefCell<HashMap<usize, TextPropertyTable>> =
        RefCell::new(HashMap::new());
}

fn add_wrapping(counter: &AtomicU64, delta: u64) {
    counter.fetch_add(delta, Ordering::Relaxed);
}

fn as_neovm_int(value: u64) -> i64 {
    value as i64
}

/// Clear all string text properties (must be called when heap changes,
/// e.g. when creating a new Context for test isolation).
pub fn reset_string_text_properties() {
    STRING_TEXT_PROPS.with(|slot| slot.borrow_mut().clear());
}

/// Collect GC roots from string text property plists.
pub fn collect_string_text_prop_gc_roots(roots: &mut Vec<Value>) {
    STRING_TEXT_PROPS.with(|slot| {
        for table in slot.borrow().values() {
            table.trace_roots(roots);
        }
    });
}

// ---------------------------------------------------------------------------
// Thread-local heap access
// ---------------------------------------------------------------------------

thread_local! {
    static CURRENT_HEAP: Cell<*mut LispHeap> = const { Cell::new(std::ptr::null_mut()) };
    /// Auto-allocated heap for tests that construct Values without an Context.
    #[cfg(test)]
    static TEST_FALLBACK_HEAP: std::cell::RefCell<Option<Box<LispHeap>>> = const { std::cell::RefCell::new(None) };
}

/// Set the current thread-local heap pointer.
/// Must be called before any Value constructors that allocate on the heap.
pub fn set_current_heap(heap: &mut LispHeap) {
    CURRENT_HEAP.with(|h| h.set(heap as *mut LispHeap));
}

/// Clear the thread-local heap pointer.
pub fn clear_current_heap() {
    CURRENT_HEAP.with(|h| h.set(std::ptr::null_mut()));
}

/// Returns true if a thread-local heap is currently set.
pub fn has_current_heap() -> bool {
    CURRENT_HEAP.with(|h| !h.get().is_null())
}

/// Save and restore the current heap pointer around a closure.
/// Used when a temporary Context is created that would overwrite the thread-local.
pub(crate) fn with_saved_heap<R>(f: impl FnOnce() -> R) -> R {
    let saved = CURRENT_HEAP.with(|h| h.get());
    let result = f();
    CURRENT_HEAP.with(|h| h.set(saved));
    result
}

/// Get raw pointer to the current heap. Panics if not set (unless in test mode,
/// where a fallback heap is auto-created).
#[inline]
pub(crate) fn current_heap_ptr() -> *mut LispHeap {
    CURRENT_HEAP.with(|h| {
        let ptr = h.get();
        if !ptr.is_null() {
            return ptr;
        }
        #[cfg(test)]
        {
            // Auto-create a heap for tests that don't use Context.
            TEST_FALLBACK_HEAP.with(|fb| {
                let mut borrow = fb.borrow_mut();
                if borrow.is_none() {
                    *borrow = Some(Box::new(LispHeap::new()));
                }
                let heap_ref: &mut LispHeap = borrow.as_mut().unwrap();
                let ptr = heap_ref as *mut LispHeap;
                h.set(ptr);
                ptr
            })
        }
        #[cfg(not(test))]
        {
            panic!("current heap not set — call set_current_heap() first");
        }
    })
}

/// Immutable access to the current thread-local heap.
///
/// # Safety
/// The returned reference is valid only for the duration of `f`.
/// Do NOT call `with_heap` or `with_heap_mut` from within `f`.
#[inline]
pub(crate) fn with_heap<R>(f: impl FnOnce(&LispHeap) -> R) -> R {
    let ptr = current_heap_ptr();
    f(unsafe { &*ptr })
}

/// Mutable access to the current thread-local heap.
///
/// # Safety
/// The returned reference is valid only for the duration of `f`.
/// Do NOT call `with_heap` or `with_heap_mut` from within `f`.
#[inline]
pub(crate) fn with_heap_mut<R>(f: impl FnOnce(&mut LispHeap) -> R) -> R {
    let ptr = current_heap_ptr();
    f(unsafe { &mut *ptr })
}

/// Snapshot of a cons cell's car and cdr values.
///
/// Returned by `read_cons()`. Used as a drop-in replacement for the
/// old `MutexGuard<ConsCell>` pattern: `pair.car` / `pair.cdr` just work.
pub struct ConsSnapshot {
    pub car: Value,
    pub cdr: Value,
}

/// A string text property run used by printed propertized-string literals.
#[derive(Clone, Debug, PartialEq)]
pub struct StringTextPropertyRun {
    pub start: usize,
    pub end: usize,
    pub plist: Value,
}

pub fn set_string_text_properties_table(id: ObjId, table: TextPropertyTable) {
    let key = obj_id_to_key(id);
    STRING_TEXT_PROPS.with(|slot| {
        let mut props = slot.borrow_mut();
        if table.is_empty() {
            props.remove(&key);
        } else {
            props.insert(key, table);
        }
    });
}

pub fn set_string_text_properties(id: ObjId, runs: Vec<StringTextPropertyRun>) {
    let mut table = TextPropertyTable::new();
    for run in &runs {
        // Convert plist Value to individual properties
        if let Some(items) = list_to_vec(&run.plist) {
            for chunk in items.chunks(2) {
                if chunk.len() == 2 {
                    if let Some(name) = chunk[0].as_symbol_name() {
                        table.put_property(run.start, run.end, name, chunk[1]);
                    }
                }
            }
        }
    }
    set_string_text_properties_table(id, table);
}

pub fn get_string_text_properties(id: ObjId) -> Option<Vec<StringTextPropertyRun>> {
    let key = obj_id_to_key(id);
    STRING_TEXT_PROPS.with(|slot| {
        let table = slot.borrow();
        let table = table.get(&key)?;
        if table.is_empty() {
            return None;
        }
        let mut runs = Vec::new();
        for interval in table.intervals_snapshot() {
            if interval.properties.is_empty() {
                continue;
            }
            let mut plist_items = Vec::new();
            for (key, val) in interval.ordered_properties() {
                plist_items.push(Value::symbol(key.to_string()));
                plist_items.push(*val);
            }
            runs.push(StringTextPropertyRun {
                start: interval.start,
                end: interval.end,
                plist: Value::list(plist_items),
            });
        }
        if runs.is_empty() { None } else { Some(runs) }
    })
}

pub fn get_string_text_properties_table(id: ObjId) -> Option<TextPropertyTable> {
    let key = obj_id_to_key(id);
    STRING_TEXT_PROPS.with(|slot| slot.borrow().get(&key).cloned())
}

fn obj_id_to_key(id: ObjId) -> usize {
    ((id.index as usize) << 32) | (id.generation as usize)
}

/// Snapshot the string text properties table (for pdump serialization).
/// Returns entries as (combined_key, table) pairs.
pub(crate) fn snapshot_string_text_props() -> Vec<(u64, TextPropertyTable)> {
    STRING_TEXT_PROPS.with(|slot| {
        slot.borrow()
            .iter()
            .map(|(&key, table)| (key as u64, table.clone()))
            .collect()
    })
}

/// Restore string text properties from a pdump snapshot.
pub(crate) fn restore_string_text_props(entries: Vec<(u64, TextPropertyTable)>) {
    STRING_TEXT_PROPS.with(|slot| {
        let mut props = slot.borrow_mut();
        props.clear();
        for (key, table) in entries {
            props.insert(key as usize, table);
        }
    });
}

/// Read car and cdr from a cons cell on the heap.
///
/// Drop-in replacement for `cell.lock().expect("poisoned")`.
#[inline]
pub fn read_cons(id: ObjId) -> ConsSnapshot {
    with_heap(|h| ConsSnapshot {
        car: h.cons_car(id),
        cdr: h.cons_cdr(id),
    })
}

// ---------------------------------------------------------------------------
// Core value types
// ---------------------------------------------------------------------------

/// Runtime Lisp value.
///
/// All heap-allocated types use `ObjId` handles into a thread-local `LispHeap`.
/// Symbol, Keyword, and Subr names use `SymId` handles into a thread-local
/// `StringInterner`, making Value `Copy` and 16 bytes.
#[derive(Clone, Copy, Debug)]
pub enum Value {
    Nil,
    /// `t` — the canonical true value.
    True,
    Int(i64),
    Float(f64, u32),
    Symbol(SymId),
    Keyword(SymId),
    Str(ObjId),
    Cons(ObjId),
    Vector(ObjId),
    Record(ObjId),
    HashTable(ObjId),
    Lambda(ObjId),
    Macro(ObjId),
    Char(char),
    /// Subr = built-in function reference (name).  Dispatched by the evaluator.
    Subr(SymId),
    /// Compiled bytecode function.
    ByteCode(ObjId),
    /// Marker object.
    Marker(ObjId),
    /// Overlay object.
    Overlay(ObjId),
    /// Buffer reference (opaque id into the BufferManager).
    Buffer(crate::buffer::BufferId),
    /// Window reference (opaque id into the FrameManager).
    Window(u64),
    /// Frame reference (opaque id into the FrameManager).
    Frame(u64),
    /// Timer reference (opaque id into the TimerManager).
    Timer(u64),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        equal_value(self, other, 0)
    }
}

/// Shared representation for lambda and macro bodies.
#[derive(Clone, Debug)]
pub struct LambdaData {
    pub params: LambdaParams,
    pub body: Rc<Vec<super::expr::Expr>>,
    /// For lexical closures: captured environment as a cons alist
    /// mirroring GNU Emacs's `Vinternal_interpreter_environment`.
    pub env: Option<Value>,
    pub docstring: Option<String>,
    /// Slot 4 in the closure vector: the `:documentation` form result.
    /// For oclosures, this is a symbol (the type name).
    /// Falls back to docstring if not set.
    pub doc_form: Option<Value>,
    /// Slot 5 in GNU Emacs's closure vector: the interactive specification.
    /// Extracted from the lambda body's `(interactive ...)` form during
    /// closure creation, matching GNU Emacs's `Ffunction` (eval.c:604-612).
    /// When present, `commandp` returns t for this closure.
    pub interactive: Option<Value>,
}

/// Describes a lambda parameter list including &optional and &rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LambdaParams {
    pub required: Vec<SymId>,
    pub optional: Vec<SymId>,
    pub rest: Option<SymId>,
}

impl LambdaParams {
    pub fn simple(names: Vec<SymId>) -> Self {
        Self {
            required: names,
            optional: Vec::new(),
            rest: None,
        }
    }

    /// Total minimum arity.
    pub fn min_arity(&self) -> usize {
        self.required.len()
    }

    /// Total maximum arity (None = unbounded due to &rest).
    pub fn max_arity(&self) -> Option<usize> {
        if self.rest.is_some() {
            None
        } else {
            Some(self.required.len() + self.optional.len())
        }
    }
}

/// Hash table with configurable test function.
#[derive(Clone, Debug)]
pub struct LispHashTable {
    pub test: HashTableTest,
    /// Symbol name provided via `:test` at construction time.
    /// For user-defined tests this preserves the alias returned by
    /// `hash-table-test`.
    pub test_name: Option<SymId>,
    pub size: i64,
    pub weakness: Option<HashTableWeakness>,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub data: HashMap<HashKey, Value>,
    /// Original key objects for diagnostics/iteration where pointer-identity
    /// keys cannot be reconstructed from `HashKey`.
    pub key_snapshots: HashMap<HashKey, Value>,
    /// Insertion order for keys — used by `maphash` to iterate in the same
    /// order as GNU Emacs (insertion order for freshly-created tables).
    pub insertion_order: Vec<HashKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashTableTest {
    Eq,
    Eql,
    Equal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashTableWeakness {
    Key,
    Value,
    KeyOrValue,
    KeyAndValue,
}

/// Key type that supports hashing for `eq`, `eql`, and `equal` tests.
/// For simplicity, we normalize keys to a hashable representation.
///
/// `Str` stores an `ObjId` and hashes/compares by string *content* via the
/// heap, avoiding a `String` clone on every `equal`-test hash lookup.
#[derive(Clone, Debug)]
pub enum HashKey {
    Nil,
    True,
    Int(i64),
    Float(u64),        // bits (for eql/equal hash tables)
    FloatEq(u64, u32), // bits + alloc ID (for eq hash tables)
    Symbol(SymId),
    Keyword(SymId),
    Str(ObjId),
    Char(char),
    Window(u64),
    Frame(u64),
    /// Pointer identity for eq hash tables (legacy, unused with ObjId migration).
    Ptr(usize),
    /// Object identity for eq hash tables (heap-allocated types).
    ObjId(u32, u32),
    /// Structural cons key for `equal`-test hash tables.
    /// Two cons cells with structurally-equal car/cdr produce equal keys.
    EqualCons(Box<HashKey>, Box<HashKey>),
    /// Structural vector/record key for `equal`-test hash tables.
    EqualVec(Vec<HashKey>),
    /// Back-reference marker used when structural objects recurse.
    Cycle(u32),
    /// Owned textual key used for structural hashing of AST-backed objects.
    Text(String),
}

impl std::hash::Hash for HashKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let tag: u8 = match self {
            HashKey::Nil => 0,
            HashKey::True => 1,
            HashKey::Int(_) => 2,
            HashKey::Float(_) => 3,
            HashKey::FloatEq(_, _) => 4,
            HashKey::Symbol(_) => 5,
            HashKey::Str(_) => 6,
            HashKey::Char(_) => 7,
            HashKey::Window(_) => 8,
            HashKey::Frame(_) => 9,
            HashKey::Ptr(_) => 10,
            HashKey::ObjId(_, _) => 11,
            HashKey::EqualCons(_, _) => 12,
            HashKey::EqualVec(_) => 13,
            HashKey::Keyword(_) => 14,
            HashKey::Cycle(_) => 15,
            HashKey::Text(_) => 16,
        };
        tag.hash(state);
        match self {
            HashKey::Nil | HashKey::True => {}
            HashKey::Int(n) => n.hash(state),
            HashKey::Float(bits) => bits.hash(state),
            HashKey::FloatEq(bits, id) => {
                bits.hash(state);
                id.hash(state);
            }
            HashKey::Symbol(id) => id.hash(state),
            HashKey::Keyword(id) => id.hash(state),
            HashKey::Str(id) => with_heap(|h| h.get_string(*id).hash(state)),
            HashKey::Char(c) => c.hash(state),
            HashKey::Window(id) | HashKey::Frame(id) => id.hash(state),
            HashKey::Ptr(p) => p.hash(state),
            HashKey::ObjId(idx, generation) => {
                idx.hash(state);
                generation.hash(state);
            }
            HashKey::EqualCons(car, cdr) => {
                car.hash(state);
                cdr.hash(state);
            }
            HashKey::EqualVec(items) => {
                items.len().hash(state);
                for item in items {
                    item.hash(state);
                }
            }
            HashKey::Cycle(index) => index.hash(state),
            HashKey::Text(text) => text.hash(state),
        }
    }
}

impl PartialEq for HashKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (HashKey::Nil, HashKey::Nil) | (HashKey::True, HashKey::True) => true,
            (HashKey::Int(a), HashKey::Int(b)) => a == b,
            (HashKey::Float(a), HashKey::Float(b)) => a == b,
            (HashKey::FloatEq(a, id_a), HashKey::FloatEq(b, id_b)) => a == b && id_a == id_b,
            (HashKey::Symbol(a), HashKey::Symbol(b)) => a == b,
            (HashKey::Keyword(a), HashKey::Keyword(b)) => a == b,
            (HashKey::Str(a), HashKey::Str(b)) => {
                a == b || with_heap(|h| h.get_string(*a) == h.get_string(*b))
            }
            (HashKey::Char(a), HashKey::Char(b)) => a == b,
            (HashKey::Window(a), HashKey::Window(b)) | (HashKey::Frame(a), HashKey::Frame(b)) => {
                a == b
            }
            (HashKey::Ptr(a), HashKey::Ptr(b)) => a == b,
            (HashKey::ObjId(ai, ag), HashKey::ObjId(bi, bg)) => ai == bi && ag == bg,
            (HashKey::EqualCons(a_car, a_cdr), HashKey::EqualCons(b_car, b_cdr)) => {
                a_car == b_car && a_cdr == b_cdr
            }
            (HashKey::EqualVec(a), HashKey::EqualVec(b)) => a == b,
            (HashKey::Cycle(a), HashKey::Cycle(b)) => a == b,
            (HashKey::Text(a), HashKey::Text(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for HashKey {}

impl HashKey {
    /// Create a `Str` hash key by allocating the string on the heap.
    pub fn from_str(s: impl Into<String>) -> Self {
        match Value::string(s) {
            Value::Str(id) => HashKey::Str(id),
            _ => unreachable!(),
        }
    }
}

impl LispHashTable {
    pub fn new(test: HashTableTest) -> Self {
        Self::new_with_options(test, 0, None, 1.5, 0.8125)
    }

    pub fn new_with_options(
        test: HashTableTest,
        size: i64,
        weakness: Option<HashTableWeakness>,
        rehash_size: f64,
        rehash_threshold: f64,
    ) -> Self {
        Self {
            test,
            test_name: None,
            size,
            weakness,
            rehash_size,
            rehash_threshold,
            data: HashMap::with_capacity(size.max(0) as usize),
            key_snapshots: HashMap::with_capacity(size.max(0) as usize),
            insertion_order: Vec::with_capacity(size.max(0) as usize),
        }
    }
}

// ---------------------------------------------------------------------------
// Value constructors
// ---------------------------------------------------------------------------

impl Value {
    pub fn t() -> Self {
        Value::True
    }

    pub fn bool(b: bool) -> Self {
        if b { Value::True } else { Value::Nil }
    }

    pub fn symbol(s: impl AsRef<str>) -> Self {
        let s = s.as_ref();
        if s == "nil" {
            Value::Nil
        } else if s == "t" {
            Value::True
        } else if s.starts_with(':') {
            // Canonicalize leading-colon names as keywords so values created
            // via `intern` and reader literals share the same representation.
            add_wrapping(&SYMBOLS_CONSED, 1);
            Value::Keyword(intern(s))
        } else {
            add_wrapping(&SYMBOLS_CONSED, 1);
            Value::Symbol(intern(s))
        }
    }

    pub fn keyword(s: impl AsRef<str>) -> Self {
        add_wrapping(&SYMBOLS_CONSED, 1);
        Value::Keyword(intern(s.as_ref()))
    }

    pub fn string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        let id = with_heap_mut(|heap| heap.alloc_string(s));
        Value::Str(id)
    }

    pub fn heap_string(s: crate::gc::types::LispString) -> Self {
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.as_str().len() as u64);
        let id = with_heap_mut(|heap| heap.alloc_lisp_string(s));
        Value::Str(id)
    }

    pub fn multibyte_string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        let id = with_heap_mut(|heap| heap.alloc_string_with_flag(s, true));
        Value::Str(id)
    }

    pub fn unibyte_string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        let id = with_heap_mut(|heap| heap.alloc_string_with_flag(s, false));
        Value::Str(id)
    }

    pub fn string_with_text_properties(
        s: impl Into<String>,
        runs: Vec<StringTextPropertyRun>,
    ) -> Self {
        let value = Self::string(s);
        if let Value::Str(id) = &value {
            set_string_text_properties(*id, runs);
        }
        value
    }

    pub fn multibyte_string_with_text_properties(
        s: impl Into<String>,
        runs: Vec<StringTextPropertyRun>,
    ) -> Self {
        let value = Self::multibyte_string(s);
        if let Value::Str(id) = &value {
            set_string_text_properties(*id, runs);
        }
        value
    }

    pub fn make_lambda(data: LambdaData) -> Self {
        let id = with_heap_mut(|heap| heap.alloc_lambda(data));
        Value::Lambda(id)
    }

    pub fn make_macro(data: LambdaData) -> Self {
        let id = with_heap_mut(|heap| heap.alloc_macro(data));
        Value::Macro(id)
    }

    pub fn make_bytecode(bc: super::bytecode::ByteCodeFunction) -> Self {
        let id = with_heap_mut(|heap| heap.alloc_bytecode(bc));
        Value::ByteCode(id)
    }

    pub fn cons(car: Value, cdr: Value) -> Self {
        add_wrapping(&CONS_CELLS_CONSED, 1);
        let id = with_heap_mut(|heap| heap.alloc_cons(car, cdr));
        Value::Cons(id)
    }

    pub fn list(values: Vec<Value>) -> Self {
        values
            .into_iter()
            .rev()
            .fold(Value::Nil, |acc, item| Value::cons(item, acc))
    }

    pub fn vector(values: Vec<Value>) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, values.len() as u64);
        let id = with_heap_mut(|heap| heap.alloc_vector(values));
        Value::Vector(id)
    }

    pub fn hash_table(test: HashTableTest) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, 1);
        let id = with_heap_mut(|heap| heap.alloc_hash_table(test));
        Value::HashTable(id)
    }

    pub fn hash_table_with_options(
        test: HashTableTest,
        size: i64,
        weakness: Option<HashTableWeakness>,
        rehash_size: f64,
        rehash_threshold: f64,
    ) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, 1);
        let id = with_heap_mut(|heap| {
            heap.alloc_hash_table_with_options(test, size, weakness, rehash_size, rehash_threshold)
        });
        Value::HashTable(id)
    }

    pub(crate) fn memory_use_counts_snapshot() -> [i64; 7] {
        [
            as_neovm_int(CONS_CELLS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(FLOATS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(VECTOR_CELLS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(SYMBOLS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(STRING_CHARS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(INTERVALS_CONSED.load(Ordering::Relaxed)),
            as_neovm_int(STRINGS_CONSED.load(Ordering::Relaxed)),
        ]
    }

    // -----------------------------------------------------------------------
    // Heap accessor methods (via thread-local)
    // -----------------------------------------------------------------------

    /// Get the car of a cons cell.
    pub fn cons_car(&self) -> Value {
        match self {
            Value::Cons(id) => with_heap(|h| h.cons_car(*id)),
            _ => panic!("cons_car on non-cons: {}", self.type_name()),
        }
    }

    /// Get the cdr of a cons cell.
    pub fn cons_cdr(&self) -> Value {
        match self {
            Value::Cons(id) => with_heap(|h| h.cons_cdr(*id)),
            _ => panic!("cons_cdr on non-cons: {}", self.type_name()),
        }
    }

    /// Set the car of a cons cell.
    pub fn set_car(&self, val: Value) {
        match self {
            Value::Cons(id) => with_heap_mut(|h| h.set_car(*id, val)),
            _ => panic!("set_car on non-cons: {}", self.type_name()),
        }
    }

    /// Set the cdr of a cons cell.
    pub fn set_cdr(&self, val: Value) {
        match self {
            Value::Cons(id) => with_heap_mut(|h| h.set_cdr(*id, val)),
            _ => panic!("set_cdr on non-cons: {}", self.type_name()),
        }
    }

    // -----------------------------------------------------------------------
    // Type predicates
    // -----------------------------------------------------------------------

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    pub fn is_truthy(&self) -> bool {
        !self.is_nil()
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Value::Nil | Value::Cons(_))
    }

    pub fn is_cons(&self) -> bool {
        matches!(self, Value::Cons(_))
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Char(_) | Value::Float(_, _))
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Char(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Value::Float(_, _))
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Value::Str(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(
            self,
            Value::Nil | Value::True | Value::Symbol(_) | Value::Keyword(_)
        )
    }

    pub fn is_keyword(&self) -> bool {
        matches!(self, Value::Keyword(_))
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, Value::Vector(_))
    }

    pub fn is_record(&self) -> bool {
        matches!(self, Value::Record(_))
    }

    pub fn is_char(&self) -> bool {
        matches!(self, Value::Char(_))
    }

    pub fn is_hash_table(&self) -> bool {
        matches!(self, Value::HashTable(_))
    }

    pub fn is_function(&self) -> bool {
        matches!(self, Value::Lambda(_) | Value::Subr(_) | Value::ByteCode(_))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "symbol",
            Value::True => "symbol",
            Value::Int(_) => "integer",
            Value::Float(_, _) => "float",
            Value::Symbol(_) => "symbol",
            Value::Keyword(_) => "symbol",
            Value::Str(_) => "string",
            Value::Cons(_) => "cons",
            Value::Vector(_) => "vector",
            Value::Record(_) => "record",
            Value::HashTable(_) => "hash-table",
            Value::Lambda(_) => "function",
            Value::Macro(_) => "macro",
            Value::Char(_) => "integer", // Emacs chars are integers
            Value::Subr(_) => "subr",
            Value::ByteCode(_) => "byte-code-function",
            Value::Marker(_) => "marker",
            Value::Overlay(_) => "overlay",
            Value::Buffer(_) => "buffer",
            Value::Window(_) => "window",
            Value::Frame(_) => "frame",
            Value::Timer(_) => "timer",
        }
    }

    /// Extract as number (int or float).  Promotes int → float if needed.
    pub fn as_number_f64(&self) -> Option<f64> {
        match self {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f, _) => Some(*f),
            Value::Char(c) => Some(*c as u32 as f64),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Char(c) => Some(*c as i64),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f, _) => Some(*f),
            _ => None,
        }
    }

    /// Borrow the string contents from the heap.
    ///
    /// # Safety
    /// The returned reference borrows from the thread-local heap.  It is valid
    /// as long as no GC collection occurs (which would free/move objects).
    /// This is safe at normal call sites because GC only runs at explicit safe
    /// points (`gc_safe_point`), never during a borrow.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(id) => {
                let ptr = current_heap_ptr();
                let heap = unsafe { &*ptr };
                Some(heap.get_string(*id))
            }
            _ => None,
        }
    }

    /// Get an owned copy of the string contents.
    pub fn as_str_owned(&self) -> Option<String> {
        self.as_str().map(|s| s.to_owned())
    }

    /// Access the heap string via a closure.
    pub fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> Option<R> {
        self.as_str().map(f)
    }

    pub fn as_symbol_name(&self) -> Option<&str> {
        match self {
            Value::Nil => Some("nil"),
            Value::True => Some("t"),
            Value::Symbol(id) => Some(resolve_sym(*id)),
            Value::Keyword(id) => Some(resolve_sym(*id)),
            _ => None,
        }
    }

    /// Check if this value is a symbol with the given name.
    /// Convenience for the common `if s == "foo"` pattern in match guards.
    pub fn is_symbol_named(&self, name: &str) -> bool {
        self.as_symbol_name() == Some(name)
    }

    /// Borrow the LambdaData from a Lambda or Macro value on the heap.
    pub fn get_lambda_data(&self) -> Option<&LambdaData> {
        let ptr = current_heap_ptr();
        let heap = unsafe { &*ptr };
        match self {
            Value::Lambda(id) => Some(heap.get_lambda(*id)),
            Value::Macro(id) => Some(heap.get_macro_data(*id)),
            _ => None,
        }
    }

    /// Borrow the ByteCodeFunction from a ByteCode value on the heap.
    pub fn get_bytecode_data(&self) -> Option<&super::bytecode::ByteCodeFunction> {
        let ptr = current_heap_ptr();
        let heap = unsafe { &*ptr };
        match self {
            Value::ByteCode(id) => Some(heap.get_bytecode(*id)),
            _ => None,
        }
    }

    /// Get the ObjId of a string value (for text property operations).
    pub fn str_id(&self) -> Option<ObjId> {
        match self {
            Value::Str(id) => Some(*id),
            _ => None,
        }
    }

    /// Convert to hash key based on the hash table test.
    pub fn to_hash_key(&self, test: &HashTableTest) -> HashKey {
        match test {
            HashTableTest::Eq => self.to_eq_key(),
            HashTableTest::Eql => self.to_eql_key(),
            HashTableTest::Equal => self.to_equal_key(),
        }
    }

    fn to_eq_key(&self) -> HashKey {
        match self {
            Value::Nil => HashKey::Nil,
            Value::True => HashKey::True,
            Value::Int(n) => HashKey::Int(*n),
            Value::Float(f, id) => HashKey::FloatEq(f.to_bits(), *id),
            Value::Symbol(id) => HashKey::Symbol(*id),
            Value::Keyword(id) => HashKey::Keyword(*id),
            // Emacs chars are integers for equality/hash semantics.
            Value::Char(c) => HashKey::Int(*c as i64),
            // All heap-allocated types: use ObjId for identity
            Value::Cons(id)
            | Value::Vector(id)
            | Value::Record(id)
            | Value::HashTable(id)
            | Value::Str(id)
            | Value::Lambda(id)
            | Value::Macro(id)
            | Value::ByteCode(id)
            | Value::Marker(id)
            | Value::Overlay(id) => HashKey::ObjId(id.index, id.generation),
            Value::Subr(id) => HashKey::Symbol(*id),
            Value::Buffer(id) => HashKey::Int(id.0 as i64),
            Value::Window(id) => HashKey::Window(*id),
            Value::Frame(id) => HashKey::Frame(*id),
            Value::Timer(id) => HashKey::Int(*id as i64),
        }
    }

    fn to_eql_key(&self) -> HashKey {
        match self {
            // eql is like eq but also does value-equality for numbers
            Value::Int(n) => HashKey::Int(*n),
            Value::Float(f, _) => HashKey::Float(f.to_bits()),
            Value::Char(c) => HashKey::Int(*c as i64),
            other => other.to_eq_key(),
        }
    }

    fn to_equal_key(&self) -> HashKey {
        let mut seen = Vec::new();
        self.to_equal_key_depth(0, &mut seen)
    }

    fn to_equal_key_depth(&self, depth: usize, seen: &mut Vec<StructuralRef>) -> HashKey {
        if depth > 200 {
            // Prevent runaway recursion on circular structures; fall back to eq.
            return self.to_eq_key();
        }
        match self {
            Value::Nil => HashKey::Nil,
            Value::True => HashKey::True,
            Value::Int(n) => HashKey::Int(*n),
            Value::Float(f, _) => HashKey::Float(f.to_bits()),
            Value::Symbol(id) => HashKey::Symbol(*id),
            Value::Keyword(id) => HashKey::Keyword(*id),
            Value::Str(id) => HashKey::Str(*id),
            Value::Char(c) => HashKey::Int(*c as i64),
            Value::Window(id) => HashKey::Window(*id),
            Value::Frame(id) => HashKey::Frame(*id),
            // Structural comparison for cons cells (critical for cl-generic memoization).
            Value::Cons(cons) => {
                if let Some(index) = seen
                    .iter()
                    .position(|entry| matches!(entry, StructuralRef::Cons(id) if id == cons))
                {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(StructuralRef::Cons(*cons));
                let pair = read_cons(*cons);
                let car_key = pair.car.to_equal_key_depth(depth + 1, seen);
                let cdr_key = pair.cdr.to_equal_key_depth(depth + 1, seen);
                seen.pop();
                HashKey::EqualCons(Box::new(car_key), Box::new(cdr_key))
            }
            // Structural comparison for vectors and records.
            Value::Vector(v) | Value::Record(v) => {
                let marker = match self {
                    Value::Vector(_) => StructuralRef::Vector(*v),
                    _ => StructuralRef::Record(*v),
                };
                if let Some(index) = seen.iter().position(|entry| *entry == marker) {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(marker);
                let items = with_heap(|h| h.get_vector(*v).clone());
                let keys: Vec<HashKey> = items
                    .iter()
                    .map(|item| item.to_equal_key_depth(depth + 1, seen))
                    .collect();
                seen.pop();
                HashKey::EqualVec(keys)
            }
            Value::Marker(id) => super::marker::marker_equal_hash_key(*id),
            Value::Lambda(id) => {
                if let Some(index) = seen
                    .iter()
                    .position(|entry| matches!(entry, StructuralRef::Lambda(other) if other == id))
                {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(StructuralRef::Lambda(*id));
                let lambda = with_heap(|h| h.get_lambda(*id).clone());
                let key = lambda_to_equal_key(&lambda, depth + 1, seen);
                seen.pop();
                key
            }
            // Functions, hash tables, etc. use identity.
            other => other.to_eq_key(),
        }
    }
}

// ---------------------------------------------------------------------------
// Equality
// ---------------------------------------------------------------------------

/// `eq` — identity comparison.
pub fn eq_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Nil, Value::Nil) => true,
        (Value::True, Value::True) => true,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a, id_a), Value::Float(b, id_b)) => {
            id_a == id_b && a.to_bits() == b.to_bits()
        }
        (Value::Int(a), Value::Char(b)) => *a == *b as i64,
        (Value::Char(a), Value::Int(b)) => *a as i64 == *b,
        (Value::Char(a), Value::Char(b)) => a == b,
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::Keyword(a), Value::Keyword(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Cons(a), Value::Cons(b)) => a == b,
        (Value::Vector(a), Value::Vector(b)) => a == b,
        (Value::Record(a), Value::Record(b)) => a == b,
        (Value::Lambda(a), Value::Lambda(b)) => a == b,
        (Value::Macro(a), Value::Macro(b)) => a == b,
        (Value::HashTable(a), Value::HashTable(b)) => a == b,
        (Value::Subr(a), Value::Subr(b)) => a == b,
        (Value::ByteCode(a), Value::ByteCode(b)) => a == b,
        (Value::Marker(a), Value::Marker(b)) => a == b,
        (Value::Overlay(a), Value::Overlay(b)) => a == b,
        (Value::Buffer(a), Value::Buffer(b)) => a == b,
        (Value::Window(a), Value::Window(b)) => a == b,
        (Value::Frame(a), Value::Frame(b)) => a == b,
        (Value::Timer(a), Value::Timer(b)) => a == b,
        _ => false,
    }
}

/// `eql` — like `eq` but also value-equality for numbers of same type.
pub fn eql_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float(a, _), Value::Float(b, _)) => a.to_bits() == b.to_bits(),
        _ => eq_value(left, right),
    }
}

/// `equal` — structural comparison.
pub fn equal_value(left: &Value, right: &Value, depth: usize) -> bool {
    let mut seen = HashSet::new();
    equal_value_inner(left, right, depth, &mut seen)
}

fn equal_value_inner(
    left: &Value,
    right: &Value,
    depth: usize,
    seen: &mut HashSet<EqualPairRef>,
) -> bool {
    if depth > 200 {
        return false;
    }
    match (left, right) {
        (Value::Nil, Value::Nil) => true,
        (Value::True, Value::True) => true,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Int(a), Value::Char(b)) => *a == *b as i64,
        (Value::Char(a), Value::Int(b)) => *a as i64 == *b,
        (Value::Float(a, _), Value::Float(b, _)) => a.to_bits() == b.to_bits(),
        (Value::Char(a), Value::Char(b)) => a == b,
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::Keyword(a), Value::Keyword(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => {
            if a == b {
                return true;
            }
            with_heap(|h| h.get_string(*a) == h.get_string(*b))
        }
        (Value::Marker(_), Value::Marker(_)) => {
            super::marker::marker_logical_fields(left)
                == super::marker::marker_logical_fields(right)
        }
        (Value::Cons(a), Value::Cons(b)) => {
            if a == b {
                return true;
            }
            let pair_ref = EqualPairRef::Cons(*a, *b);
            if !seen.insert(pair_ref) {
                return true;
            }
            let a_car = with_heap(|h| h.cons_car(*a));
            let a_cdr = with_heap(|h| h.cons_cdr(*a));
            let b_car = with_heap(|h| h.cons_car(*b));
            let b_cdr = with_heap(|h| h.cons_cdr(*b));
            equal_value_inner(&a_car, &b_car, depth + 1, seen)
                && equal_value_inner(&a_cdr, &b_cdr, depth + 1, seen)
        }
        (Value::Vector(a), Value::Vector(b)) | (Value::Record(a), Value::Record(b)) => {
            if a == b {
                return true;
            }
            let pair_ref = match (left, right) {
                (Value::Vector(_), Value::Vector(_)) => EqualPairRef::Vector(*a, *b),
                _ => EqualPairRef::Record(*a, *b),
            };
            if !seen.insert(pair_ref) {
                return true;
            }
            // Copy element pairs out (Value is Copy) to compare outside the borrow.
            // Returns None if lengths differ, Some(pairs) otherwise.
            let pairs: Option<Vec<(Value, Value)>> = with_heap(|h| {
                let av = h.get_vector(*a);
                let bv = h.get_vector(*b);
                if av.len() != bv.len() {
                    return None;
                }
                Some(av.iter().copied().zip(bv.iter().copied()).collect())
            });
            matches!(pairs, Some(ref p) if p
                .iter()
                .all(|(x, y)| equal_value_inner(x, y, depth + 1, seen)))
        }
        // Hash tables: identity comparison only (same as eq), matching GNU Emacs
        // where PVEC_HASH_TABLE < PVEC_CLOSURE causes early return false.
        (Value::HashTable(a), Value::HashTable(b)) => a == b,
        (Value::Lambda(a), Value::Lambda(b)) => {
            if a == b {
                return true;
            }
            let pair_ref = EqualPairRef::Lambda(*a, *b);
            if !seen.insert(pair_ref) {
                return true;
            }
            let (left_lambda, right_lambda) =
                with_heap(|h| (h.get_lambda(*a).clone(), h.get_lambda(*b).clone()));
            lambda_data_equal(&left_lambda, &right_lambda, depth + 1, seen)
        }
        (Value::Macro(a), Value::Macro(b)) => a == b,
        (Value::Subr(a), Value::Subr(b)) => a == b,
        (Value::ByteCode(a), Value::ByteCode(b)) => a == b,
        (Value::Overlay(a), Value::Overlay(b)) => a == b,
        (Value::Buffer(a), Value::Buffer(b)) => a == b,
        (Value::Window(a), Value::Window(b)) => a == b,
        (Value::Frame(a), Value::Frame(b)) => a == b,
        (Value::Timer(a), Value::Timer(b)) => a == b,
        _ => false,
    }
}

fn lambda_to_equal_key(
    lambda: &LambdaData,
    depth: usize,
    seen: &mut Vec<StructuralRef>,
) -> HashKey {
    if depth > 200 {
        return HashKey::Text("#<lambda-depth-limit>".to_string());
    }

    let mut params =
        Vec::with_capacity(lambda.params.required.len() + lambda.params.optional.len() + 3);
    params.push(HashKey::Text("params".to_string()));
    for sym in &lambda.params.required {
        params.push(HashKey::Symbol(*sym));
    }
    if !lambda.params.optional.is_empty() {
        params.push(HashKey::Text("&optional".to_string()));
        for sym in &lambda.params.optional {
            params.push(HashKey::Symbol(*sym));
        }
    }
    if let Some(rest) = lambda.params.rest {
        params.push(HashKey::Text("&rest".to_string()));
        params.push(HashKey::Symbol(rest));
    }

    let mut slots = vec![
        HashKey::Text("lambda".to_string()),
        HashKey::EqualVec(params),
        HashKey::EqualVec(
            lambda
                .body
                .iter()
                .map(|expr| HashKey::Text(super::expr::print_expr(expr)))
                .collect(),
        ),
        match lambda.env {
            Some(env) => env.to_equal_key_depth(depth + 1, seen),
            None => HashKey::Text("dynamic".to_string()),
        },
    ];

    if lambda.docstring.is_some() || lambda.doc_form.is_some() {
        slots.push(HashKey::Nil);
        let doc = if let Some(doc_form) = lambda.doc_form {
            doc_form.to_equal_key_depth(depth + 1, seen)
        } else if let Some(docstring) = &lambda.docstring {
            HashKey::Text(docstring.clone())
        } else {
            HashKey::Nil
        };
        slots.push(doc);
    }

    HashKey::EqualVec(slots)
}

fn lambda_data_equal(
    left: &LambdaData,
    right: &LambdaData,
    depth: usize,
    seen: &mut HashSet<EqualPairRef>,
) -> bool {
    if left.params != right.params || left.body.as_ref() != right.body.as_ref() {
        return false;
    }

    let env_equal = match (left.env, right.env) {
        (None, None) => true,
        (Some(l), Some(r)) => equal_value_inner(&l, &r, depth + 1, seen),
        _ => false,
    };
    if !env_equal || left.docstring != right.docstring {
        return false;
    }

    match (left.doc_form, right.doc_form) {
        (None, None) => true,
        (Some(l), Some(r)) => equal_value_inner(&l, &r, depth + 1, seen),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StructuralRef {
    Cons(ObjId),
    Vector(ObjId),
    Record(ObjId),
    Lambda(ObjId),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum EqualPairRef {
    Cons(ObjId, ObjId),
    Vector(ObjId, ObjId),
    Record(ObjId, ObjId),
    Lambda(ObjId, ObjId),
}

// ---------------------------------------------------------------------------
// List iteration helpers
// ---------------------------------------------------------------------------

/// Collect a proper list into a Vec.  Returns None if not a proper list or
/// circular list.  Uses tortoise-and-hare cycle detection.
pub fn list_to_vec(value: &Value) -> Option<Vec<Value>> {
    let mut result = Vec::new();
    let mut tortoise = *value;
    let mut hare = *value;
    let mut step = 0u64;
    loop {
        match hare {
            Value::Nil => return Some(result),
            Value::Cons(id) => {
                result.push(with_heap(|h| h.cons_car(id)));
                hare = with_heap(|h| h.cons_cdr(id));
                step += 1;
                // Advance tortoise every other step
                if step % 2 == 0 {
                    if let Value::Cons(tid) = tortoise {
                        tortoise = with_heap(|h| h.cons_cdr(tid));
                    }
                    if tortoise == hare {
                        return None; // cycle detected
                    }
                }
            }
            _ => return None,
        }
    }
}

/// Length of a list (counts cons cells).  Returns None if improper or circular
/// list detected.  Uses tortoise-and-hare cycle detection (like GNU Emacs
/// `FOR_EACH_TAIL_SAFE`).
pub fn list_length(value: &Value) -> Option<usize> {
    let mut len = 0;
    let mut tortoise = *value;
    let mut hare = *value;
    loop {
        match hare {
            Value::Nil => return Some(len),
            Value::Cons(id) => {
                len += 1;
                hare = with_heap(|h| h.cons_cdr(id));
                // Advance hare a second step
                match hare {
                    Value::Nil => return Some(len),
                    Value::Cons(id2) => {
                        len += 1;
                        hare = with_heap(|h| h.cons_cdr(id2));
                    }
                    _ => return None, // improper
                }
                // Advance tortoise one step
                if let Value::Cons(tid) = tortoise {
                    tortoise = with_heap(|h| h.cons_cdr(tid));
                }
                // Cycle detection: if tortoise == hare, it's circular
                if tortoise == hare {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", super::print::print_value(self))
    }
}

// ---------------------------------------------------------------------------
// Flat cons-alist lexical environment helpers
// ---------------------------------------------------------------------------

/// Walk a cons-alist lexenv for a symbol.  Returns the `ObjId` of the
/// `(sym . val)` cons cell, or `None` if not found.
pub fn lexenv_assq(lexenv: Value, sym_id: SymId) -> Option<ObjId> {
    let mut cursor = lexenv;
    loop {
        match cursor {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                // Elements are either (sym . val) lexical bindings, bare
                // symbols declaring local dynamic scope, or the GNU top-level
                // lexical sentinel `(t)`.
                if let Value::Cons(binding) = pair.car {
                    let bp = read_cons(binding);
                    if let Some(s) = lexenv_binding_symbol_id(bp.car)
                        && s == sym_id
                    {
                        return Some(binding);
                    }
                }
                cursor = pair.cdr;
            }
            _ => return None,
        }
    }
}

fn lexenv_binding_symbol_id(value: Value) -> Option<SymId> {
    match value {
        Value::Symbol(sym) => Some(sym),
        Value::True => Some(intern("t")),
        Value::Nil => Some(intern("nil")),
        _ => None,
    }
}

fn lexenv_binding_symbol_value(sym_id: SymId) -> Value {
    match resolve_sym(sym_id) {
        "t" => Value::True,
        "nil" => Value::Nil,
        _ => Value::Symbol(sym_id),
    }
}

/// Look up symbol value in a cons-alist lexenv.
pub fn lexenv_lookup(lexenv: Value, sym_id: SymId) -> Option<Value> {
    lexenv_assq(lexenv, sym_id).map(|cell| read_cons(cell).cdr)
}

/// Return true if the lexical environment contains a bare-symbol declaration
/// marking SYM_ID as locally special/dynamic.
pub fn lexenv_declares_special(lexenv: Value, sym_id: SymId) -> bool {
    let mut cursor = lexenv;
    loop {
        match cursor {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if let Value::Symbol(s) = pair.car
                    && s == sym_id
                {
                    return true;
                }
                cursor = pair.cdr;
            }
            _ => return false,
        }
    }
}

/// Collect bare-symbol entries from the lexical environment. GNU Emacs uses
/// these to propagate local `defvar` declarations into `macroexp--dynvars`
/// during macro expansion.
pub fn lexenv_bare_symbols(lexenv: Value) -> Vec<SymId> {
    let mut cursor = lexenv;
    let mut symbols = Vec::new();
    loop {
        match cursor {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if let Value::Symbol(s) = pair.car {
                    symbols.push(s);
                }
                cursor = pair.cdr;
            }
            _ => return symbols,
        }
    }
}

/// Mutate a binding in place: set cdr of the `(sym . val)` cons cell.
pub fn lexenv_set(cell_id: ObjId, value: Value) {
    with_heap_mut(|h| h.set_cdr(cell_id, value));
}

/// Prepend a `(sym . val)` binding onto a lexenv alist.  Returns the new head.
pub fn lexenv_prepend(lexenv: Value, sym_id: SymId, val: Value) -> Value {
    let binding = Value::cons(lexenv_binding_symbol_value(sym_id), val);
    Value::cons(binding, lexenv)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "value_test.rs"]
mod tests;
