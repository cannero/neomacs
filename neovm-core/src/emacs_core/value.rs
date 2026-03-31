//! Lisp value representation and fundamental operations.
//!
//! After the tagged pointer migration, `Value` is a type alias for
//! `TaggedValue`.  This module provides:
//!
//! - The `Value` type alias and re-exports of `ValueKind`, `VecLikeType`
//! - Convenience constructors that allocate on the thread-local heap
//! - Data types: `LambdaData`, `LambdaParams`, `LispHashTable`, `HashKey`, etc.
//! - Equality functions: `eq_value`, `eql_value`, `equal_value`
//! - List helpers: `list_to_vec`, `list_length`
//! - Lexical environment helpers: `lexenv_*`
//! - String text property helpers

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::intern::{SymId, intern, resolve_sym};
use crate::buffer::text_props::TextPropertyTable;
use crate::gc::GcTrace;
use crate::gc::types::LispString;
use crate::tagged::gc::with_tagged_heap;
use crate::tagged::header::{
    BufferObj, ByteCodeObj, FloatObj, FrameObj, HashTableObj, LambdaObj, MacroObj, MarkerObj,
    OverlayObj, RecordObj, StringObj, TimerObj, VecLikeHeader, VectorObj, WindowObj,
};
use crate::tagged::value::TaggedValue;

// ---------------------------------------------------------------------------
// The Value type — now a tagged pointer
// ---------------------------------------------------------------------------

/// Runtime Lisp value.
///
/// This is a type alias for `TaggedValue` — a single machine word (8 bytes on
/// 64-bit) encoding type and payload via tag bits.  Pattern matching uses
/// `value.kind()` → `ValueKind`.
pub type Value = TaggedValue;

// Re-export tagged types for downstream use.
pub use crate::tagged::header::VecLikeType;
pub use crate::tagged::value::ValueKind;

// ---------------------------------------------------------------------------
// Data structures (unchanged — not part of the Value enum)
// ---------------------------------------------------------------------------

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
        if self.entries.len() != other.entries.len() {
            return false;
        }
        self.entries
            .iter()
            .zip(other.entries.iter())
            .all(|((k1, v1), (k2, v2))| k1 == k2 && eq_value(v1, v2))
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

// ---------------------------------------------------------------------------
// Allocation statistics counters (unchanged)
// ---------------------------------------------------------------------------

const ZERO_COUNT: u64 = 0;

static CONS_CELLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static FLOATS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static VECTOR_CELLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static SYMBOLS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static STRING_CHARS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static INTERVALS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);
static STRINGS_CONSED: AtomicU64 = AtomicU64::new(ZERO_COUNT);

fn add_wrapping(counter: &AtomicU64, delta: u64) {
    counter.fetch_add(delta, Ordering::Relaxed);
}

fn as_neovm_int(value: u64) -> i64 {
    value as i64
}

// ---------------------------------------------------------------------------
// String text properties (keyed by pointer address now)
// ---------------------------------------------------------------------------

thread_local! {
    static STRING_TEXT_PROPS: RefCell<HashMap<usize, TextPropertyTable>> =
        RefCell::new(HashMap::new());
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

/// Get the text property key for a string value (pointer address).
fn string_text_prop_key(value: Value) -> Option<usize> {
    value.as_string_ptr().map(|p| p as usize)
}

pub fn set_string_text_properties_table_for_value(value: Value, table: TextPropertyTable) {
    if let Some(key) = string_text_prop_key(value) {
        STRING_TEXT_PROPS.with(|slot| {
            let mut props = slot.borrow_mut();
            if table.is_empty() {
                props.remove(&key);
            } else {
                props.insert(key, table);
            }
        });
    }
}

pub fn set_string_text_properties_for_value(value: Value, runs: Vec<StringTextPropertyRun>) {
    let mut table = TextPropertyTable::new();
    for run in &runs {
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
    set_string_text_properties_table_for_value(value, table);
}

pub fn get_string_text_properties_for_value(value: Value) -> Option<Vec<StringTextPropertyRun>> {
    let key = string_text_prop_key(value)?;
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
                plist_items.push(Value::make_symbol(key.to_string()));
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

pub fn get_string_text_properties_table_for_value(value: Value) -> Option<TextPropertyTable> {
    let key = string_text_prop_key(value)?;
    STRING_TEXT_PROPS.with(|slot| slot.borrow().get(&key).cloned())
}

/// Snapshot the string text properties table (for pdump serialization).
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

// ---------------------------------------------------------------------------
// Legacy ObjId-based text property API (backward compat during migration)
// ---------------------------------------------------------------------------

use crate::gc::heap::LispHeap;
use crate::gc::types::ObjId;
use std::cell::Cell;

thread_local! {
    static CURRENT_HEAP: Cell<*mut LispHeap> = const { Cell::new(std::ptr::null_mut()) };
    #[cfg(test)]
    static TEST_FALLBACK_HEAP: std::cell::RefCell<Option<Box<LispHeap>>> = const { std::cell::RefCell::new(None) };
}

/// Set the current thread-local heap pointer.
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
pub(crate) fn with_saved_heap<R>(f: impl FnOnce() -> R) -> R {
    let saved = CURRENT_HEAP.with(|h| h.get());
    let result = f();
    CURRENT_HEAP.with(|h| h.set(saved));
    result
}

/// Get raw pointer to the current heap.
#[inline]
pub(crate) fn current_heap_ptr() -> *mut LispHeap {
    CURRENT_HEAP.with(|h| {
        let ptr = h.get();
        if !ptr.is_null() {
            return ptr;
        }
        #[cfg(test)]
        {
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
#[inline]
pub fn with_heap<R>(f: impl FnOnce(&LispHeap) -> R) -> R {
    let ptr = current_heap_ptr();
    f(unsafe { &*ptr })
}

/// Mutable access to the current thread-local heap.
#[inline]
pub(crate) fn with_heap_mut<R>(f: impl FnOnce(&mut LispHeap) -> R) -> R {
    let ptr = current_heap_ptr();
    f(unsafe { &mut *ptr })
}

// Legacy ObjId-based text property functions (still used during migration)
fn obj_id_to_key(id: ObjId) -> usize {
    ((id.index as usize) << 32) | (id.generation as usize)
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
                plist_items.push(Value::make_symbol(key.to_string()));
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

/// Snapshot of a cons cell's car and cdr values.
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

/// Read car and cdr from a cons cell on the heap (legacy ObjId version).
#[inline]
pub fn read_cons(id: ObjId) -> ConsSnapshot {
    with_heap(|h| ConsSnapshot {
        car: h.cons_car(id),
        cdr: h.cons_cdr(id),
    })
}

/// Allocate a fresh float identity (legacy — no longer needed with tagged pointers).
pub fn next_float_id() -> u32 {
    0 // Stub: float identity is now pointer-based
}

// ---------------------------------------------------------------------------
// Overlay ObjId <-> TaggedValue bridge (migration compat)
// ---------------------------------------------------------------------------

thread_local! {
    /// Maps overlay tagged-pointer address → legacy ObjId.
    static OVERLAY_OBJ_ID_MAP: RefCell<HashMap<usize, ObjId>> = RefCell::new(HashMap::new());
}

/// Register a mapping from a tagged overlay Value to its legacy ObjId.
pub fn register_overlay_obj_id(value: Value, id: ObjId) {
    let key = value.0; // raw tagged pointer
    OVERLAY_OBJ_ID_MAP.with(|m| m.borrow_mut().insert(key, id));
}

/// Look up the legacy ObjId for a tagged overlay Value.
pub fn lookup_overlay_obj_id(value: &Value) -> Option<ObjId> {
    let key = value.0;
    OVERLAY_OBJ_ID_MAP.with(|m| m.borrow().get(&key).copied())
}

/// Create a tagged Value from a legacy overlay ObjId.
///
/// Clones the OverlayData from the old heap, allocates a new overlay on the
/// tagged heap, and registers the mapping so `lookup_overlay_obj_id` works.
pub fn overlay_id_to_value(id: ObjId) -> Value {
    let data = with_heap(|h| h.get_overlay(id).clone());
    let value = Value::make_overlay(data);
    register_overlay_obj_id(value, id);
    value
}

// ---------------------------------------------------------------------------
// LambdaData, LambdaParams
// ---------------------------------------------------------------------------

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
    pub doc_form: Option<Value>,
    /// Slot 5 in GNU Emacs's closure vector: the interactive specification.
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

    pub fn min_arity(&self) -> usize {
        self.required.len()
    }

    pub fn max_arity(&self) -> Option<usize> {
        if self.rest.is_some() {
            None
        } else {
            Some(self.required.len() + self.optional.len())
        }
    }
}

// ---------------------------------------------------------------------------
// LispHashTable, HashKey
// ---------------------------------------------------------------------------

/// Hash table with configurable test function.
#[derive(Clone, Debug)]
pub struct LispHashTable {
    pub test: HashTableTest,
    pub test_name: Option<SymId>,
    pub size: i64,
    pub weakness: Option<HashTableWeakness>,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub data: HashMap<HashKey, Value>,
    pub key_snapshots: HashMap<HashKey, Value>,
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
#[derive(Clone, Debug)]
pub enum HashKey {
    Nil,
    True,
    Int(i64),
    Float(u64),
    FloatEq(u64, u32),
    Symbol(SymId),
    Keyword(SymId),
    Char(char),
    Window(u64),
    Frame(u64),
    /// Pointer identity for eq hash tables (tagged pointer bits).
    Ptr(usize),
    /// Structural cons key for `equal`-test hash tables.
    EqualCons(Box<HashKey>, Box<HashKey>),
    /// Structural vector/record key for `equal`-test hash tables.
    EqualVec(Vec<HashKey>),
    /// Back-reference marker used when structural objects recurse.
    Cycle(u32),
    /// Owned textual key used for structural hashing.
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
            HashKey::Char(_) => 7,
            HashKey::Window(_) => 8,
            HashKey::Frame(_) => 9,
            HashKey::Ptr(_) => 10,
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
            HashKey::Symbol(id) | HashKey::Keyword(id) => id.hash(state),
            HashKey::Char(c) => c.hash(state),
            HashKey::Window(id) | HashKey::Frame(id) => id.hash(state),
            HashKey::Ptr(p) => p.hash(state),
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
            (HashKey::Char(a), HashKey::Char(b)) => a == b,
            (HashKey::Window(a), HashKey::Window(b)) | (HashKey::Frame(a), HashKey::Frame(b)) => {
                a == b
            }
            (HashKey::Ptr(a), HashKey::Ptr(b)) => a == b,
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
    /// Create a string hash key by allocating on the heap.
    pub fn from_str(s: impl Into<String>) -> Self {
        // For `equal` hash tables, use text content directly
        HashKey::Text(s.into())
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
// Conversion traits for flexible constructors
// ---------------------------------------------------------------------------

/// Trait for types that can be converted to a symbol Value.
/// Implemented by `&str`, `String`, `SymId`.
pub trait IntoSymbol {
    fn into_symbol(self) -> Value;
}

impl IntoSymbol for SymId {
    fn into_symbol(self) -> Value {
        TaggedValue::from_sym_id(self)
    }
}

impl IntoSymbol for &str {
    fn into_symbol(self) -> Value {
        if self == "nil" {
            Value::NIL
        } else if self == "t" {
            Value::T
        } else if self.starts_with(':') {
            add_wrapping(&SYMBOLS_CONSED, 1);
            TaggedValue::from_kw_id(intern(self))
        } else {
            add_wrapping(&SYMBOLS_CONSED, 1);
            TaggedValue::from_sym_id(intern(self))
        }
    }
}

impl IntoSymbol for String {
    fn into_symbol(self) -> Value {
        self.as_str().into_symbol()
    }
}

impl IntoSymbol for &String {
    fn into_symbol(self) -> Value {
        self.as_str().into_symbol()
    }
}

impl IntoSymbol for &&str {
    fn into_symbol(self) -> Value {
        (*self).into_symbol()
    }
}

impl IntoSymbol for &&String {
    fn into_symbol(self) -> Value {
        self.as_str().into_symbol()
    }
}

/// Trait for types that can be converted to a keyword Value.
pub trait IntoKeyword {
    fn into_keyword(self) -> Value;
}

impl IntoKeyword for SymId {
    fn into_keyword(self) -> Value {
        TaggedValue::from_kw_id(self)
    }
}

impl IntoKeyword for &str {
    fn into_keyword(self) -> Value {
        add_wrapping(&SYMBOLS_CONSED, 1);
        TaggedValue::from_kw_id(intern(self))
    }
}

impl IntoKeyword for String {
    fn into_keyword(self) -> Value {
        self.as_str().into_keyword()
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors on TaggedValue
// ---------------------------------------------------------------------------

impl TaggedValue {
    /// Create a symbol from a string (with nil/t/keyword canonicalization) or SymId.
    pub fn symbol(s: impl IntoSymbol) -> Self {
        s.into_symbol()
    }

    /// Create a symbol by interning a name string, with nil/t/keyword canonicalization.
    pub fn make_symbol(s: impl AsRef<str>) -> Self {
        s.as_ref().into_symbol()
    }

    /// Create a keyword from a string or SymId.
    pub fn keyword(s: impl IntoKeyword) -> Self {
        s.into_keyword()
    }

    /// Create a keyword by interning a name string.
    pub fn make_keyword(s: impl AsRef<str>) -> Self {
        s.as_ref().into_keyword()
    }

    /// Convert bool to Value (T or NIL).
    #[inline]
    pub fn bool(b: bool) -> Self {
        if b { Value::T } else { Value::NIL }
    }

    // -- Heap-allocating constructors --

    /// Allocate a string on the heap (old API name).
    pub fn string(s: impl Into<String>) -> Self {
        Self::make_string(s)
    }

    /// Allocate a string on the heap.
    pub fn make_string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        with_tagged_heap(|h| h.alloc_string(LispString::new(s, true)))
    }

    /// Allocate a string from a pre-built LispString.
    pub fn heap_string(s: LispString) -> Self {
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.as_str().len() as u64);
        with_tagged_heap(|h| h.alloc_string(s))
    }

    /// Allocate a multibyte string.
    pub fn multibyte_string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        with_tagged_heap(|h| h.alloc_string(LispString::new(s, true)))
    }

    /// Allocate a unibyte string.
    pub fn unibyte_string(s: impl Into<String>) -> Self {
        let s = s.into();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        with_tagged_heap(|h| h.alloc_string(LispString::new(s, false)))
    }

    /// Allocate a string with text properties.
    pub fn string_with_text_properties(
        s: impl Into<String>,
        runs: Vec<StringTextPropertyRun>,
    ) -> Self {
        let value = Self::make_string(s);
        set_string_text_properties_for_value(value, runs);
        value
    }

    /// Allocate a multibyte string with text properties.
    pub fn multibyte_string_with_text_properties(
        s: impl Into<String>,
        runs: Vec<StringTextPropertyRun>,
    ) -> Self {
        let value = Self::multibyte_string(s);
        set_string_text_properties_for_value(value, runs);
        value
    }

    /// Allocate a float on the heap.
    pub fn make_float(f: f64) -> Self {
        add_wrapping(&FLOATS_CONSED, 1);
        with_tagged_heap(|h| h.alloc_float(f))
    }

    /// Allocate a cons cell (old API name).
    pub fn cons(car: Value, cdr: Value) -> Self {
        Self::make_cons(car, cdr)
    }

    /// Allocate a cons cell.
    pub fn make_cons(car: Value, cdr: Value) -> Self {
        add_wrapping(&CONS_CELLS_CONSED, 1);
        with_tagged_heap(|h| h.alloc_cons(car, cdr))
    }

    /// Build a proper list from a Vec.
    pub fn list(values: Vec<Value>) -> Self {
        values
            .into_iter()
            .rev()
            .fold(Value::NIL, |acc, item| Value::cons(item, acc))
    }

    /// Allocate a vector (old API name).
    pub fn vector(values: Vec<Value>) -> Self {
        Self::make_vector(values)
    }

    /// Allocate a vector.
    pub fn make_vector(values: Vec<Value>) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, values.len() as u64);
        with_tagged_heap(|h| h.alloc_vector(values))
    }

    /// Allocate a record.
    pub fn make_record(values: Vec<Value>) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, values.len() as u64);
        with_tagged_heap(|h| h.alloc_record(values))
    }

    /// Allocate a lambda.
    pub fn make_lambda(data: LambdaData) -> Self {
        with_tagged_heap(|h| h.alloc_lambda(data))
    }

    /// Allocate a macro.
    pub fn make_macro(data: LambdaData) -> Self {
        with_tagged_heap(|h| h.alloc_macro(data))
    }

    /// Allocate a bytecode function.
    pub fn make_bytecode(bc: super::bytecode::ByteCodeFunction) -> Self {
        with_tagged_heap(|h| h.alloc_bytecode(bc))
    }

    /// Allocate a hash table.
    pub fn hash_table(test: HashTableTest) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, 1);
        with_tagged_heap(|h| h.alloc_hash_table(LispHashTable::new(test)))
    }

    /// Allocate a hash table with options.
    pub fn hash_table_with_options(
        test: HashTableTest,
        size: i64,
        weakness: Option<HashTableWeakness>,
        rehash_size: f64,
        rehash_threshold: f64,
    ) -> Self {
        add_wrapping(&VECTOR_CELLS_CONSED, 1);
        with_tagged_heap(|h| {
            h.alloc_hash_table(LispHashTable::new_with_options(
                test,
                size,
                weakness,
                rehash_size,
                rehash_threshold,
            ))
        })
    }

    /// Allocate a marker.
    pub fn make_marker(data: crate::gc::types::MarkerData) -> Self {
        with_tagged_heap(|h| h.alloc_marker(data))
    }

    /// Allocate an overlay.
    pub fn make_overlay(data: crate::gc::types::OverlayData) -> Self {
        with_tagged_heap(|h| h.alloc_overlay(data))
    }

    /// Allocate a buffer reference.
    pub fn make_buffer(id: crate::buffer::BufferId) -> Self {
        let obj = Box::new(BufferObj {
            header: VecLikeHeader::new(VecLikeType::Buffer),
            id,
        });
        let ptr = Box::into_raw(obj);
        with_tagged_heap(|h| {
            // Link into GC list
            unsafe {
                (*ptr).header.gc.next = std::ptr::null_mut();
            }
            h.allocated_count += 1;
            unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
        })
    }

    /// Allocate a window reference.
    pub fn make_window(id: u64) -> Self {
        let obj = Box::new(WindowObj {
            header: VecLikeHeader::new(VecLikeType::Window),
            id,
        });
        let ptr = Box::into_raw(obj);
        with_tagged_heap(|h| {
            h.allocated_count += 1;
            unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
        })
    }

    /// Allocate a frame reference.
    pub fn make_frame(id: u64) -> Self {
        let obj = Box::new(FrameObj {
            header: VecLikeHeader::new(VecLikeType::Frame),
            id,
        });
        let ptr = Box::into_raw(obj);
        with_tagged_heap(|h| {
            h.allocated_count += 1;
            unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
        })
    }

    /// Allocate a timer reference.
    pub fn make_timer(id: u64) -> Self {
        let obj = Box::new(TimerObj {
            header: VecLikeHeader::new(VecLikeType::Timer),
            id,
        });
        let ptr = Box::into_raw(obj);
        with_tagged_heap(|h| {
            h.allocated_count += 1;
            unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
        })
    }

    // -- Veclike accessor helpers --

    /// Check if this is a lambda.
    #[inline]
    pub fn is_lambda(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Lambda)
    }

    /// Check if this is a macro.
    #[inline]
    pub fn is_macro(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Macro)
    }

    /// Check if this is a bytecode function.
    #[inline]
    pub fn is_bytecode(self) -> bool {
        self.veclike_type() == Some(VecLikeType::ByteCode)
    }

    /// Check if this is a buffer.
    #[inline]
    pub fn is_buffer(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Buffer)
    }

    /// Check if this is a window.
    #[inline]
    pub fn is_window(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Window)
    }

    /// Check if this is a frame.
    #[inline]
    pub fn is_frame(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Frame)
    }

    /// Check if this is a timer.
    #[inline]
    pub fn is_timer(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Timer)
    }

    /// Check if this is a marker.
    #[inline]
    pub fn is_marker(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Marker)
    }

    /// Check if this is an overlay.
    #[inline]
    pub fn is_overlay(self) -> bool {
        self.veclike_type() == Some(VecLikeType::Overlay)
    }

    // -- Data accessors for heap types --

    /// Get an owned copy of the string contents.
    pub fn as_str_owned(self) -> Option<String> {
        self.as_str().map(|s| s.to_owned())
    }

    /// Access the heap string via a closure.
    pub fn with_str<R>(self, f: impl FnOnce(&str) -> R) -> Option<R> {
        self.as_str().map(f)
    }

    /// Borrow the LispString for a string value.
    pub fn as_lisp_string(self) -> Option<&'static LispString> {
        self.as_string_ptr()
            .map(|p| unsafe { &(*p).data })
    }

    /// Check if a string is multibyte.
    pub fn string_is_multibyte(self) -> bool {
        self.as_lisp_string().map_or(false, |s| s.multibyte)
    }

    /// Borrow the LambdaData from a Lambda or Macro value.
    pub fn get_lambda_data(self) -> Option<&'static LambdaData> {
        match self.veclike_type()? {
            VecLikeType::Lambda => {
                let ptr = self.as_veclike_ptr().unwrap() as *const LambdaObj;
                Some(unsafe { &(*ptr).data })
            }
            VecLikeType::Macro => {
                let ptr = self.as_veclike_ptr().unwrap() as *const MacroObj;
                Some(unsafe { &(*ptr).data })
            }
            _ => None,
        }
    }

    /// Borrow the ByteCodeFunction from a ByteCode value.
    pub fn get_bytecode_data(self) -> Option<&'static super::bytecode::ByteCodeFunction> {
        if self.veclike_type()? == VecLikeType::ByteCode {
            let ptr = self.as_veclike_ptr().unwrap() as *const ByteCodeObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Get the pointer address as a unique identity for a string value.
    /// Used for text property operations.
    pub fn str_ptr_key(self) -> Option<usize> {
        self.as_string_ptr().map(|p| p as usize)
    }

    /// Get the buffer ID from a buffer value.
    pub fn as_buffer_id(self) -> Option<crate::buffer::BufferId> {
        if self.is_buffer() {
            let ptr = self.as_veclike_ptr().unwrap() as *const BufferObj;
            Some(unsafe { (*ptr).id })
        } else {
            None
        }
    }

    /// Get the window ID from a window value.
    pub fn as_window_id(self) -> Option<u64> {
        if self.is_window() {
            let ptr = self.as_veclike_ptr().unwrap() as *const WindowObj;
            Some(unsafe { (*ptr).id })
        } else {
            None
        }
    }

    /// Get the frame ID from a frame value.
    pub fn as_frame_id(self) -> Option<u64> {
        if self.is_frame() {
            let ptr = self.as_veclike_ptr().unwrap() as *const FrameObj;
            Some(unsafe { (*ptr).id })
        } else {
            None
        }
    }

    /// Get the timer ID from a timer value.
    pub fn as_timer_id(self) -> Option<u64> {
        if self.is_timer() {
            let ptr = self.as_veclike_ptr().unwrap() as *const TimerObj;
            Some(unsafe { (*ptr).id })
        } else {
            None
        }
    }

    /// Get the marker data from a marker value.
    pub fn as_marker_data(self) -> Option<&'static crate::gc::types::MarkerData> {
        if self.is_marker() {
            let ptr = self.as_veclike_ptr().unwrap() as *const MarkerObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Get mutable marker data from a marker value.
    pub fn as_marker_data_mut(self) -> Option<&'static mut crate::gc::types::MarkerData> {
        if self.is_marker() {
            let ptr = self.as_veclike_ptr().unwrap() as *mut MarkerObj;
            Some(unsafe { &mut (*ptr).data })
        } else {
            None
        }
    }

    /// Get the overlay data from an overlay value.
    pub fn as_overlay_data(self) -> Option<&'static crate::gc::types::OverlayData> {
        if self.is_overlay() {
            let ptr = self.as_veclike_ptr().unwrap() as *const OverlayObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Get vector elements.
    pub fn as_vector_data(self) -> Option<&'static Vec<Value>> {
        if self.is_vector() {
            let ptr = self.as_veclike_ptr().unwrap() as *const VectorObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Get mutable vector elements.
    pub fn as_vector_data_mut(self) -> Option<&'static mut Vec<Value>> {
        if self.is_vector() {
            let ptr = self.as_veclike_ptr().unwrap() as *mut VectorObj;
            Some(unsafe { &mut (*ptr).data })
        } else {
            None
        }
    }

    /// Get record elements.
    pub fn as_record_data(self) -> Option<&'static Vec<Value>> {
        if self.is_record() {
            let ptr = self.as_veclike_ptr().unwrap() as *const RecordObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Get mutable record elements.
    pub fn as_record_data_mut(self) -> Option<&'static mut Vec<Value>> {
        if self.is_record() {
            let ptr = self.as_veclike_ptr().unwrap() as *mut RecordObj;
            Some(unsafe { &mut (*ptr).data })
        } else {
            None
        }
    }

    /// Get hash table reference.
    pub fn as_hash_table(self) -> Option<&'static LispHashTable> {
        if self.is_hash_table() {
            let ptr = self.as_veclike_ptr().unwrap() as *const HashTableObj;
            Some(unsafe { &(*ptr).table })
        } else {
            None
        }
    }

    /// Get mutable hash table reference.
    pub fn as_hash_table_mut(self) -> Option<&'static mut LispHashTable> {
        if self.is_hash_table() {
            let ptr = self.as_veclike_ptr().unwrap() as *mut HashTableObj;
            Some(unsafe { &mut (*ptr).table })
        } else {
            None
        }
    }

    /// Get mutable lambda data reference.
    pub fn get_lambda_data_mut(self) -> Option<&'static mut LambdaData> {
        match self.veclike_type()? {
            VecLikeType::Lambda => {
                let ptr = self.as_veclike_ptr().unwrap() as *mut LambdaObj;
                Some(unsafe { &mut (*ptr).data })
            }
            VecLikeType::Macro => {
                let ptr = self.as_veclike_ptr().unwrap() as *mut MacroObj;
                Some(unsafe { &mut (*ptr).data })
            }
            _ => None,
        }
    }

    /// Get mutable bytecode data reference.
    pub fn get_bytecode_data_mut(self) -> Option<&'static mut super::bytecode::ByteCodeFunction> {
        if self.veclike_type()? == VecLikeType::ByteCode {
            let ptr = self.as_veclike_ptr().unwrap() as *mut ByteCodeObj;
            Some(unsafe { &mut (*ptr).data })
        } else {
            None
        }
    }

    /// Get mutable string data reference.
    pub fn as_lisp_string_mut(self) -> Option<&'static mut LispString> {
        self.as_string_ptr()
            .map(|p| unsafe { &mut (*(p as *mut StringObj)).data })
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
        match self.kind() {
            ValueKind::Nil => HashKey::Nil,
            ValueKind::T => HashKey::True,
            ValueKind::Fixnum(n) => HashKey::Int(n),
            ValueKind::Float => {
                // For eq, each float allocation is unique (pointer identity)
                HashKey::Ptr(self.bits())
            }
            ValueKind::Symbol(id) => HashKey::Symbol(id),
            ValueKind::Keyword(id) => HashKey::Keyword(id),
            ValueKind::Char(c) => HashKey::Int(c as i64),
            ValueKind::Subr(id) => HashKey::Symbol(id),
            // All heap types: use pointer identity
            ValueKind::Cons | ValueKind::String | ValueKind::Veclike(_) => {
                HashKey::Ptr(self.bits())
            }
            ValueKind::Unknown => HashKey::Ptr(self.bits()),
        }
    }

    fn to_eql_key(&self) -> HashKey {
        match self.kind() {
            ValueKind::Fixnum(n) => HashKey::Int(n),
            ValueKind::Float => HashKey::Float(self.xfloat().to_bits()),
            ValueKind::Char(c) => HashKey::Int(c as i64),
            _ => self.to_eq_key(),
        }
    }

    fn to_equal_key(&self) -> HashKey {
        let mut seen = Vec::new();
        self.to_equal_key_depth(0, &mut seen)
    }

    fn to_equal_key_depth(&self, depth: usize, seen: &mut Vec<usize>) -> HashKey {
        if depth > 200 {
            return self.to_eq_key();
        }
        match self.kind() {
            ValueKind::Nil => HashKey::Nil,
            ValueKind::T => HashKey::True,
            ValueKind::Fixnum(n) => HashKey::Int(n),
            ValueKind::Float => HashKey::Float(self.xfloat().to_bits()),
            ValueKind::Symbol(id) => HashKey::Symbol(id),
            ValueKind::Keyword(id) => HashKey::Keyword(id),
            ValueKind::String => {
                // Use content for equal hashing
                if let Some(s) = self.as_str() {
                    HashKey::Text(s.to_string())
                } else {
                    self.to_eq_key()
                }
            }
            ValueKind::Char(c) => HashKey::Int(c as i64),
            ValueKind::Cons => {
                let ptr = self.bits();
                if let Some(index) = seen.iter().position(|&p| p == ptr) {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(ptr);
                let car = self.cons_car();
                let cdr = self.cons_cdr();
                let car_key = car.to_equal_key_depth(depth + 1, seen);
                let cdr_key = cdr.to_equal_key_depth(depth + 1, seen);
                seen.pop();
                HashKey::EqualCons(Box::new(car_key), Box::new(cdr_key))
            }
            ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
                let ptr = self.bits();
                if let Some(index) = seen.iter().position(|&p| p == ptr) {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(ptr);
                let items = if self.is_vector() {
                    self.as_vector_data().unwrap().clone()
                } else {
                    self.as_record_data().unwrap().clone()
                };
                let keys: Vec<HashKey> = items
                    .iter()
                    .map(|item| item.to_equal_key_depth(depth + 1, seen))
                    .collect();
                seen.pop();
                HashKey::EqualVec(keys)
            }
            ValueKind::Veclike(VecLikeType::Marker) => {
                super::marker::marker_equal_hash_key_value(self)
            }
            ValueKind::Veclike(VecLikeType::Lambda) => {
                let ptr = self.bits();
                if let Some(index) = seen.iter().position(|&p| p == ptr) {
                    return HashKey::Cycle(index as u32);
                }
                seen.push(ptr);
                let lambda = self.get_lambda_data().unwrap().clone();
                let key = lambda_to_equal_key(&lambda, depth + 1, seen);
                seen.pop();
                key
            }
            _ => self.to_eq_key(),
        }
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
}

// ---------------------------------------------------------------------------
// Equality
// ---------------------------------------------------------------------------

/// `eq` — identity comparison (pointer equality for heap types).
pub fn eq_value(left: &Value, right: &Value) -> bool {
    // For tagged pointers, eq is just bitwise comparison,
    // EXCEPT that Char values should be eq to Fixnums with same numeric value.
    if left.bits() == right.bits() {
        return true;
    }
    // Cross-type char/int eq
    match (left.kind(), right.kind()) {
        (ValueKind::Fixnum(a), ValueKind::Char(b)) => a == b as i64,
        (ValueKind::Char(a), ValueKind::Fixnum(b)) => a as i64 == b,
        _ => false,
    }
}

/// `eql` — like `eq` but also value-equality for numbers of same type.
pub fn eql_value(left: &Value, right: &Value) -> bool {
    if left.bits() == right.bits() {
        return true;
    }
    match (left.kind(), right.kind()) {
        (ValueKind::Float, ValueKind::Float) => left.xfloat().to_bits() == right.xfloat().to_bits(),
        (ValueKind::Fixnum(a), ValueKind::Char(b)) => a == b as i64,
        (ValueKind::Char(a), ValueKind::Fixnum(b)) => a as i64 == b,
        _ => false,
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
    seen: &mut HashSet<(usize, usize)>,
) -> bool {
    if depth > 200 {
        return false;
    }
    // Fast path: bitwise equal
    if left.bits() == right.bits() {
        return true;
    }
    match (left.kind(), right.kind()) {
        (ValueKind::Nil, ValueKind::Nil) => true,
        (ValueKind::T, ValueKind::T) => true,
        (ValueKind::Fixnum(a), ValueKind::Fixnum(b)) => a == b,
        (ValueKind::Fixnum(a), ValueKind::Char(b)) => a == b as i64,
        (ValueKind::Char(a), ValueKind::Fixnum(b)) => a as i64 == b,
        (ValueKind::Float, ValueKind::Float) => {
            left.xfloat().to_bits() == right.xfloat().to_bits()
        }
        (ValueKind::Char(a), ValueKind::Char(b)) => a == b,
        (ValueKind::Symbol(a), ValueKind::Symbol(b)) => a == b,
        (ValueKind::Keyword(a), ValueKind::Keyword(b)) => a == b,
        (ValueKind::String, ValueKind::String) => left.as_str() == right.as_str(),
        (ValueKind::Veclike(VecLikeType::Marker), ValueKind::Veclike(VecLikeType::Marker)) => {
            super::marker::marker_logical_fields(left)
                == super::marker::marker_logical_fields(right)
        }
        (ValueKind::Cons, ValueKind::Cons) => {
            let pair = (left.bits(), right.bits());
            if !seen.insert(pair) {
                return true;
            }
            let a_car = left.cons_car();
            let a_cdr = left.cons_cdr();
            let b_car = right.cons_car();
            let b_cdr = right.cons_cdr();
            equal_value_inner(&a_car, &b_car, depth + 1, seen)
                && equal_value_inner(&a_cdr, &b_cdr, depth + 1, seen)
        }
        (ValueKind::Veclike(VecLikeType::Vector), ValueKind::Veclike(VecLikeType::Vector))
        | (ValueKind::Veclike(VecLikeType::Record), ValueKind::Veclike(VecLikeType::Record)) => {
            let pair = (left.bits(), right.bits());
            if !seen.insert(pair) {
                return true;
            }
            let av = left.as_vector_data().or_else(|| left.as_record_data());
            let bv = right.as_vector_data().or_else(|| right.as_record_data());
            match (av, bv) {
                (Some(a), Some(b)) => {
                    if a.len() != b.len() {
                        return false;
                    }
                    a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| equal_value_inner(x, y, depth + 1, seen))
                }
                _ => false,
            }
        }
        (ValueKind::Veclike(VecLikeType::HashTable), ValueKind::Veclike(VecLikeType::HashTable)) => {
            left.bits() == right.bits()
        }
        (ValueKind::Veclike(VecLikeType::Lambda), ValueKind::Veclike(VecLikeType::Lambda)) => {
            let pair = (left.bits(), right.bits());
            if !seen.insert(pair) {
                return true;
            }
            let left_lambda = left.get_lambda_data().unwrap().clone();
            let right_lambda = right.get_lambda_data().unwrap().clone();
            lambda_data_equal(&left_lambda, &right_lambda, depth + 1, seen)
        }
        (ValueKind::Subr(a), ValueKind::Subr(b)) => a == b,
        // For all other same-type veclike comparisons, use identity
        (ValueKind::Veclike(a), ValueKind::Veclike(b)) if a == b => {
            left.bits() == right.bits()
        }
        _ => false,
    }
}

fn lambda_to_equal_key(
    lambda: &LambdaData,
    depth: usize,
    seen: &mut Vec<usize>,
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
            Some(env) => env.to_equal_key_depth(0, seen),
            None => HashKey::Text("dynamic".to_string()),
        },
    ];

    if lambda.docstring.is_some() || lambda.doc_form.is_some() {
        slots.push(HashKey::Nil);
        let doc = if let Some(doc_form) = lambda.doc_form {
            doc_form.to_equal_key_depth(0, seen)
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
    seen: &mut HashSet<(usize, usize)>,
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

// ---------------------------------------------------------------------------
// List iteration helpers
// ---------------------------------------------------------------------------

/// Collect a proper list into a Vec.
pub fn list_to_vec(value: &Value) -> Option<Vec<Value>> {
    let mut result = Vec::new();
    let mut tortoise = *value;
    let mut hare = *value;
    let mut step = 0u64;
    loop {
        if hare.is_nil() {
            return Some(result);
        } else if hare.is_cons() {
            result.push(hare.cons_car());
            hare = hare.cons_cdr();
            step += 1;
            if step % 2 == 0 {
                if tortoise.is_cons() {
                    tortoise = tortoise.cons_cdr();
                }
                if tortoise.bits() == hare.bits() {
                    return None; // cycle
                }
            }
        } else {
            return None;
        }
    }
}

/// Length of a list (counts cons cells).
pub fn list_length(value: &Value) -> Option<usize> {
    let mut len = 0;
    let mut tortoise = *value;
    let mut hare = *value;
    loop {
        if hare.is_nil() {
            return Some(len);
        } else if hare.is_cons() {
            len += 1;
            hare = hare.cons_cdr();
            if hare.is_nil() {
                return Some(len);
            } else if hare.is_cons() {
                len += 1;
                hare = hare.cons_cdr();
            } else {
                return None; // improper
            }
            if tortoise.is_cons() {
                tortoise = tortoise.cons_cdr();
            }
            if tortoise.bits() == hare.bits() {
                return None; // cycle
            }
        } else {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl fmt::Display for TaggedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", super::print::print_value(self))
    }
}

// ---------------------------------------------------------------------------
// Flat cons-alist lexical environment helpers
// ---------------------------------------------------------------------------

/// Walk a cons-alist lexenv for a symbol. Returns the cons cell Value
/// of the `(sym . val)` binding, or `None` if not found.
pub fn lexenv_assq(lexenv: Value, sym_id: SymId) -> Option<Value> {
    let mut cursor = lexenv;
    loop {
        if cursor.is_cons() {
            let car = cursor.cons_car();
            if car.is_cons() {
                let binding_sym = car.cons_car();
                if let Some(s) = lexenv_binding_symbol_id(binding_sym) {
                    if s == sym_id {
                        return Some(car);
                    }
                }
            }
            cursor = cursor.cons_cdr();
        } else {
            return None;
        }
    }
}

fn lexenv_binding_symbol_id(value: Value) -> Option<SymId> {
    match value.kind() {
        ValueKind::Symbol(sym) => Some(sym),
        ValueKind::T => Some(intern("t")),
        ValueKind::Nil => Some(intern("nil")),
        _ => None,
    }
}

fn lexenv_binding_symbol_value(sym_id: SymId) -> Value {
    match resolve_sym(sym_id) {
        "t" => Value::T,
        "nil" => Value::NIL,
        _ => TaggedValue::from_sym_id(sym_id),
    }
}

/// Look up symbol value in a cons-alist lexenv.
pub fn lexenv_lookup(lexenv: Value, sym_id: SymId) -> Option<Value> {
    lexenv_assq(lexenv, sym_id).map(|cell| cell.cons_cdr())
}

/// Return true if the lexical environment contains a bare-symbol declaration
/// marking SYM_ID as locally special/dynamic.
pub fn lexenv_declares_special(lexenv: Value, sym_id: SymId) -> bool {
    let mut cursor = lexenv;
    loop {
        if cursor.is_cons() {
            let car = cursor.cons_car();
            if let Some(id) = car.as_symbol_id() {
                if id == sym_id {
                    return true;
                }
            }
            cursor = cursor.cons_cdr();
        } else {
            return false;
        }
    }
}

/// Collect bare-symbol entries from the lexical environment.
pub fn lexenv_bare_symbols(lexenv: Value) -> Vec<SymId> {
    let mut cursor = lexenv;
    let mut symbols = Vec::new();
    loop {
        if cursor.is_cons() {
            let car = cursor.cons_car();
            if let Some(s) = car.as_symbol_id() {
                symbols.push(s);
            }
            cursor = cursor.cons_cdr();
        } else {
            return symbols;
        }
    }
}

/// Mutate a binding in place: set cdr of the `(sym . val)` cons cell.
pub fn lexenv_set(cell: Value, value: Value) {
    cell.set_cdr(value);
}

/// Prepend a `(sym . val)` binding onto a lexenv alist. Returns the new head.
pub fn lexenv_prepend(lexenv: Value, sym_id: SymId, val: Value) -> Value {
    let binding = Value::make_cons(lexenv_binding_symbol_value(sym_id), val);
    Value::make_cons(binding, lexenv)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "value_test.rs"]
mod tests;
