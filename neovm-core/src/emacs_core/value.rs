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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::intern::{SymId, intern, resolve_sym};
use crate::buffer::text_props::TextPropertyTable;
use crate::gc_trace::GcTrace;
use crate::heap_types::LispString;
use crate::tagged::gc::with_tagged_heap;
use crate::tagged::header::{
    BufferObj, ByteCodeObj, FloatObj, FrameObj, HashTableObj, LambdaObj, MacroObj, MarkerObj,
    OverlayObj, RecordObj, StringObj, TimerObj, VecLikeHeader, VectorObj, WindowObj,
};
use crate::tagged::mutate;
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
// String text properties
// ---------------------------------------------------------------------------

fn string_text_props(value: Value) -> Option<&'static TextPropertyTable> {
    let ptr = value.as_string_ptr()? as *const StringObj;
    Some(unsafe { &(*ptr).text_props })
}

/// String text properties now live on the string object itself.
///
/// Heap resets automatically discard them with the owning string, so there is
/// no side table to clear anymore.
pub fn reset_string_text_properties() {}

/// String text property GC roots are traced from `StringObj` during heap mark.
pub fn collect_string_text_prop_gc_roots(_roots: &mut Vec<Value>) {}

pub fn set_string_text_properties_table_for_value(value: Value, table: TextPropertyTable) {
    let _ = mutate::with_string_text_props_mut(value, |props| {
        *props = table;
    });
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
    let table = string_text_props(value)?;
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
}

pub fn string_has_text_properties_for_value(value: Value) -> bool {
    string_text_props(value).is_some_and(|table| !table.is_empty())
}

pub fn get_string_text_properties_table_for_value(value: Value) -> Option<TextPropertyTable> {
    let table = string_text_props(value)?;
    if table.is_empty() {
        None
    } else {
        Some(table.clone())
    }
}

/// A string text property run used by printed propertized-string literals.
#[derive(Clone, Debug, PartialEq)]
pub struct StringTextPropertyRun {
    pub start: usize,
    pub end: usize,
    pub plist: Value,
}

/// Snapshot of a cons cell's car and cdr values (legacy compatibility).
pub struct ConsSnapshot {
    pub car: Value,
    pub cdr: Value,
}

/// Allocate a fresh float identity (stub — float identity is pointer-based).
pub fn next_float_id() -> u32 {
    0
}

// ---------------------------------------------------------------------------
// LambdaData, LambdaParams
// ---------------------------------------------------------------------------

/// Shared representation for lambda and macro bodies.
#[derive(Clone, Debug)]
pub struct LambdaData {
    pub params: LambdaParams,
    /// Body forms as a list of Values (Lisp forms to evaluate).
    pub body: Vec<Value>,
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

use crate::tagged::header::{
    CLOSURE_ARGLIST, CLOSURE_CODE, CLOSURE_CONSTANTS, CLOSURE_DOC_STRING, CLOSURE_INTERACTIVE,
    CLOSURE_MIN_SLOTS, CLOSURE_STACK_DEPTH,
};

impl LambdaData {
    /// Convert LambdaData to a GNU-compatible closure slot vector.
    ///
    /// Layout: [arglist, body, env, depth, docstring, interactive]
    /// All slots are GC-managed Values.
    pub fn to_closure_slots(&self) -> Vec<Value> {
        // Slot 0: arglist as Lisp list
        let arglist = crate::emacs_core::builtins::lambda_params_to_value(&self.params);

        // Slot 1: body as Lisp list of forms
        let body = Value::list(self.body.clone());

        // Slot 2: lexical environment (or nil for dynamic)
        let env = match self.env {
            Some(env_val) if env_val.is_nil() => Value::list(vec![Value::T]),
            Some(env_val) => env_val,
            None => Value::NIL,
        };

        // Slot 3: stack depth (nil for interpreted)
        let depth = Value::NIL;

        // Slot 4: docstring
        let doc = self
            .doc_form
            .or_else(|| self.docstring.as_ref().map(|d| Value::string(d.clone())))
            .unwrap_or(Value::NIL);

        // Slot 5: interactive spec
        let interactive = self.interactive.unwrap_or(Value::NIL);

        vec![arglist, body, env, depth, doc, interactive]
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

pub(crate) fn build_hash_table_literal_value(
    test: HashTableTest,
    test_name: Option<SymId>,
    size: i64,
    weakness: Option<HashTableWeakness>,
    rehash_size: f64,
    rehash_threshold: f64,
    entries: Vec<(Value, Value)>,
) -> Value {
    let table_value =
        Value::hash_table_with_options(test, size, weakness, rehash_size, rehash_threshold);
    let _ = table_value.with_hash_table_mut(|table| {
        table.test_name = test_name;
        for (key_value, val_value) in entries {
            let key = key_value.to_hash_key(&table.test);
            let inserting_new_key = !table.data.contains_key(&key);
            table.data.insert(key.clone(), val_value);
            if inserting_new_key {
                table.key_snapshots.insert(key.clone(), key_value);
                table.insertion_order.push(key);
            }
        }
    });
    table_value
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

fn canonical_keyword_name(name: &str) -> String {
    if name.starts_with(':') {
        name.to_owned()
    } else {
        format!(":{name}")
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

    /// Create a keyword by interning a canonical `:name` symbol.
    pub fn keyword(s: impl AsRef<str>) -> Self {
        add_wrapping(&SYMBOLS_CONSED, 1);
        TaggedValue::from_kw_id(intern(&canonical_keyword_name(s.as_ref())))
    }

    /// Wrap an existing interned keyword symbol id.
    ///
    /// Callers must only pass SymIds whose canonical names already start with `:`.
    pub fn keyword_id(id: SymId) -> Self {
        TaggedValue::from_kw_id(id)
    }

    /// Create a keyword by interning a name string.
    pub fn make_keyword(s: impl AsRef<str>) -> Self {
        Self::keyword(s)
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
    /// ASCII-only strings are created as unibyte (matching GNU Emacs
    /// where make_string with pure ASCII is effectively unibyte).
    /// Non-ASCII strings are created as multibyte.
    pub fn make_string(s: impl Into<String>) -> Self {
        let s = s.into();
        let multibyte = !s.is_ascii();
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.len() as u64);
        with_tagged_heap(|h| h.alloc_string(LispString::new(s, multibyte)))
    }

    /// Allocate a string from a pre-built LispString.
    pub fn heap_string(s: LispString) -> Self {
        add_wrapping(&STRINGS_CONSED, 1);
        add_wrapping(&STRING_CHARS_CONSED, s.sbytes() as u64);
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

    /// Allocate a bignum on the heap. Caller is responsible for ensuring
    /// the value is outside fixnum range — internal callers should
    /// almost always use [`Value::make_integer`] instead, which mirrors
    /// GNU `make_integer_mpz` (`src/bignum.c:146`) by returning a
    /// fixnum when the value fits and only allocating a bignum on
    /// promotion.
    pub fn bignum(value: rug::Integer) -> Self {
        with_tagged_heap(|h| h.alloc_bignum(value))
    }

    /// Canonical "make a Lisp integer from this rug::Integer" entry
    /// point. Mirrors GNU `make_integer_mpz` (`src/bignum.c:146`):
    /// returns a fixnum if the value fits in fixnum range, otherwise
    /// allocates a bignum object.
    pub fn make_integer(value: rug::Integer) -> Self {
        if let Some(small) = value.to_i64() {
            if (TaggedValue::MOST_NEGATIVE_FIXNUM..=TaggedValue::MOST_POSITIVE_FIXNUM)
                .contains(&small)
            {
                return Self::fixnum(small);
            }
        }
        Self::bignum(value)
    }

    /// Convenience used by the dump loader to materialize a bignum from
    /// its decimal representation. If parsing fails (which would
    /// indicate a corrupt dump) it falls back to 0 rather than
    /// panicking — the dump format guarantees a valid base-10 string.
    pub fn make_integer_from_str_or_zero(text: &str) -> Self {
        match rug::Integer::parse(text) {
            Ok(incomplete) => Self::make_integer(rug::Integer::from(incomplete)),
            Err(_) => Self::fixnum(0),
        }
    }

    /// Allocate a cons cell (old API name).
    pub fn cons(car: Value, cdr: Value) -> Self {
        Self::make_cons(car, cdr)
    }

    /// Allocate a cons cell.
    pub fn make_cons(car: Value, cdr: Value) -> Self {
        // Validate string values aren't corrupt before storing in cons
        if car.is_string() {
            let ptr = car.as_string_ptr().unwrap();
            let hdr = unsafe { &(*(ptr as *const crate::tagged::header::StringObj)).header };
            if !matches!(hdr.kind, crate::tagged::header::HeapObjectKind::String) {
                // Check if the address is actually a VecLike — dump its type_tag
                let vlh = unsafe { &*(ptr as *const crate::tagged::header::VecLikeHeader) };
                let expected_tagged = ptr as usize | 0b011; // what the VecLike tag would be
                panic!(
                    "CONS CAR BUG: car={:#x} (ptr {:?}, kind={:?}) is corrupt string.\n\
                     VecLikeHeader.type_tag={:?}\n\
                     If this were tagged as VecLike it would be {:#x}\n\
                     car XOR veclike_tagged = {:#x}",
                    car.0,
                    ptr,
                    hdr.kind,
                    vlh.type_tag,
                    expected_tagged,
                    car.0 ^ expected_tagged,
                );
            }
        }
        add_wrapping(&CONS_CELLS_CONSED, 1);
        with_tagged_heap(|h| h.alloc_cons(car, cdr))
    }

    /// Build a proper list from a Vec.
    pub fn list(mut values: Vec<Value>) -> Self {
        let mut acc = Value::NIL;
        while let Some(item) = values.pop() {
            acc = Value::cons(item, acc);
        }
        acc
    }

    /// Build a proper list from a slice without first cloning into a `Vec`.
    pub fn list_from_slice(values: &[Value]) -> Self {
        let mut acc = Value::NIL;
        let mut idx = values.len();
        while idx > 0 {
            idx -= 1;
            acc = Value::cons(values[idx], acc);
        }
        acc
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

    /// Allocate a lambda. Converts LambdaData to a Value vector for GC safety.
    pub fn make_lambda(data: LambdaData) -> Self {
        with_tagged_heap(|h| h.alloc_lambda_from_data(data))
    }

    /// Allocate a lambda from already-validated GNU closure slots.
    pub fn make_lambda_with_slots(slots: Vec<Value>) -> Self {
        with_tagged_heap(|h| h.alloc_lambda(slots))
    }

    /// Allocate a macro. Converts LambdaData to a Value vector for GC safety.
    pub fn make_macro(data: LambdaData) -> Self {
        with_tagged_heap(|h| h.alloc_macro_from_data(data))
    }

    /// Allocate a macro from already-validated GNU closure slots.
    pub fn make_macro_with_slots(slots: Vec<Value>) -> Self {
        with_tagged_heap(|h| h.alloc_macro(slots))
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
    pub fn make_marker(data: crate::heap_types::MarkerData) -> Self {
        with_tagged_heap(|h| h.alloc_marker(data))
    }

    /// Allocate an overlay.
    pub fn make_overlay(data: crate::heap_types::OverlayData) -> Self {
        with_tagged_heap(|h| h.alloc_overlay(data))
    }

    /// Allocate a buffer reference.
    pub fn make_buffer(id: crate::buffer::BufferId) -> Self {
        with_tagged_heap(|h| {
            if let Some(value) = h.buffer_value(id) {
                value
            } else {
                let value = h.alloc_buffer(id);
                h.register_buffer_value(id, value);
                value
            }
        })
    }

    /// Allocate a window reference.
    pub fn make_window(id: u64) -> Self {
        with_tagged_heap(|h| {
            if let Some(value) = h.window_value(id) {
                value
            } else {
                let value = h.alloc_window(id);
                h.register_window_value(id, value);
                value
            }
        })
    }

    /// Allocate a frame reference.
    pub fn make_frame(id: u64) -> Self {
        with_tagged_heap(|h| {
            if let Some(value) = h.frame_value(id) {
                value
            } else {
                let value = h.alloc_frame(id);
                h.register_frame_value(id, value);
                value
            }
        })
    }

    /// Allocate a timer reference.
    pub fn make_timer(id: u64) -> Self {
        with_tagged_heap(|h| {
            if let Some(value) = h.timer_value(id) {
                value
            } else {
                let value = h.alloc_timer(id);
                h.register_timer_value(id, value);
                value
            }
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

    /// Get an owned runtime-string view of a Lisp string, preserving raw
    /// unibyte bytes and multibyte Emacs encoding instead of requiring UTF-8.
    pub fn as_runtime_string_owned(self) -> Option<String> {
        self.as_lisp_string().map(|string| {
            crate::emacs_core::string_escape::emacs_bytes_to_storage_string(
                string.as_bytes(),
                string.is_multibyte(),
            )
        })
    }

    /// Access the heap string via a closure.
    pub fn with_str<R>(self, f: impl FnOnce(&str) -> R) -> Option<R> {
        self.as_str().map(f)
    }

    /// Borrow the LispString for a string value.
    pub fn as_lisp_string(self) -> Option<&'static LispString> {
        self.as_string_ptr().map(|p| unsafe { &(*p).data })
    }

    /// Check if a string is multibyte.
    pub fn string_is_multibyte(self) -> bool {
        self.as_lisp_string().map_or(false, |s| s.is_multibyte())
    }

    /// Get the closure slot vector for a Lambda or Macro.
    pub fn closure_slots(self) -> Option<&'static Vec<Value>> {
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

    /// Mutate closure slots through the centralized tagged-runtime write path.
    pub fn with_closure_slots_mut<R>(self, f: impl FnOnce(&mut Vec<Value>) -> R) -> Option<R> {
        mutate::with_closure_slots_mut(self, f)
    }

    /// Replace the entire closure slot vector through the centralized write path.
    pub fn replace_closure_slots(self, slots: Vec<Value>) -> bool {
        mutate::replace_closure_slots(self, slots)
    }

    /// Update a single closure slot through the centralized write path.
    pub fn set_closure_slot(self, index: usize, value: Value) -> bool {
        mutate::set_closure_slot(self, index, value)
    }

    fn closure_parsed_params_cell(self) -> Option<&'static OnceLock<LambdaParams>> {
        match self.veclike_type()? {
            VecLikeType::Lambda => {
                let ptr = self.as_veclike_ptr().unwrap() as *const LambdaObj;
                Some(unsafe { &(*ptr).parsed_params })
            }
            VecLikeType::Macro => {
                let ptr = self.as_veclike_ptr().unwrap() as *const MacroObj;
                Some(unsafe { &(*ptr).parsed_params })
            }
            _ => None,
        }
    }

    pub fn closure_slot(self, index: usize) -> Option<Value> {
        self.closure_slots()
            .and_then(|slots| slots.get(index).copied())
    }

    pub fn closure_params(self) -> Option<&'static LambdaParams> {
        let cell = self.closure_parsed_params_cell()?;
        Some(cell.get_or_init(|| {
            let arglist = self.closure_slot(CLOSURE_ARGLIST).unwrap_or(Value::NIL);
            crate::emacs_core::builtins::parse_lambda_params_from_value(&arglist)
                .unwrap_or_else(|_| LambdaParams::simple(vec![]))
        }))
    }

    pub fn closure_body_value(self) -> Option<Value> {
        self.closure_slot(CLOSURE_CODE)
    }

    pub fn closure_env(self) -> Option<Option<Value>> {
        self.closure_slot(CLOSURE_CONSTANTS)
            .map(|env| (!env.is_nil()).then_some(env))
    }

    pub fn closure_doc_value(self) -> Option<Value> {
        self.closure_slot(CLOSURE_DOC_STRING)
    }

    pub fn closure_doc_form(self) -> Option<Option<Value>> {
        self.closure_doc_value().map(|doc| {
            if doc.is_nil() || doc.is_string() {
                None
            } else {
                Some(doc)
            }
        })
    }

    pub fn closure_docstring(self) -> Option<Option<&'static str>> {
        self.closure_doc_value().map(|doc| doc.as_str())
    }

    pub fn closure_interactive(self) -> Option<Option<Value>> {
        self.closure_slot(CLOSURE_INTERACTIVE)
            .map(|interactive| (!interactive.is_nil()).then_some(interactive))
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
    pub fn as_marker_data(self) -> Option<&'static crate::heap_types::MarkerData> {
        if self.is_marker() {
            let ptr = self.as_veclike_ptr().unwrap() as *const MarkerObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Mutate marker data through the centralized tagged-runtime write path.
    pub fn with_marker_data_mut<R>(
        self,
        f: impl FnOnce(&mut crate::heap_types::MarkerData) -> R,
    ) -> Option<R> {
        mutate::with_marker_data_mut(self, f)
    }

    /// Get the overlay data from an overlay value.
    pub fn as_overlay_data(self) -> Option<&'static crate::heap_types::OverlayData> {
        if self.is_overlay() {
            let ptr = self.as_veclike_ptr().unwrap() as *const OverlayObj;
            Some(unsafe { &(*ptr).data })
        } else {
            None
        }
    }

    /// Mutate overlay data through the centralized tagged-runtime write path.
    pub fn with_overlay_data_mut<R>(
        self,
        f: impl FnOnce(&mut crate::heap_types::OverlayData) -> R,
    ) -> Option<R> {
        mutate::with_overlay_data_mut(self, f)
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

    /// Mutate vector elements through the centralized tagged-runtime write path.
    pub fn with_vector_data_mut<R>(self, f: impl FnOnce(&mut Vec<Value>) -> R) -> Option<R> {
        mutate::with_vector_data_mut(self, f)
    }

    /// Replace the entire contents of a vector value.
    pub fn replace_vector_data(self, values: Vec<Value>) -> bool {
        mutate::replace_vector_data(self, values)
    }

    /// Update a single vector slot through the centralized write path.
    pub fn set_vector_slot(self, index: usize, value: Value) -> bool {
        mutate::set_vector_slot(self, index, value)
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

    /// Mutate record elements through the centralized tagged-runtime write path.
    pub fn with_record_data_mut<R>(self, f: impl FnOnce(&mut Vec<Value>) -> R) -> Option<R> {
        mutate::with_record_data_mut(self, f)
    }

    /// Replace the entire contents of a record value.
    pub fn replace_record_data(self, values: Vec<Value>) -> bool {
        mutate::replace_record_data(self, values)
    }

    /// Update a single record slot through the centralized write path.
    pub fn set_record_slot(self, index: usize, value: Value) -> bool {
        mutate::set_record_slot(self, index, value)
    }

    /// Replace the contents of either a vector or record.
    pub fn replace_vectorlike_sequence_data(self, values: Vec<Value>) -> bool {
        match self.veclike_type() {
            Some(VecLikeType::Vector) => self.replace_vector_data(values),
            Some(VecLikeType::Record) => self.replace_record_data(values),
            _ => false,
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

    /// Mutate a hash table through the centralized tagged-runtime write path.
    pub fn with_hash_table_mut<R>(self, f: impl FnOnce(&mut LispHashTable) -> R) -> Option<R> {
        mutate::with_hash_table_mut(self, f)
    }

    /// Replace the entire contents of a hash table value.
    pub fn replace_hash_table(self, table: LispHashTable) -> bool {
        self.with_hash_table_mut(|current| *current = table)
            .is_some()
    }

    /// Mutate bytecode data through the centralized tagged-runtime write path.
    pub fn with_bytecode_data_mut<R>(
        self,
        f: impl FnOnce(&mut super::bytecode::ByteCodeFunction) -> R,
    ) -> Option<R> {
        mutate::with_bytecode_data_mut(self, f)
    }

    /// Mutate string data through the centralized tagged-runtime write path.
    pub fn with_lisp_string_mut<R>(self, f: impl FnOnce(&mut LispString) -> R) -> Option<R> {
        mutate::with_lisp_string_mut(self, f)
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
            // All heap types: use pointer identity
            ValueKind::Cons | ValueKind::String | ValueKind::Veclike(_) => {
                HashKey::Ptr(self.bits())
            }
            // `Qunbound` collapses to its unique bit pattern — two
            // UNBOUND values are `eq`. Ordinary Lisp code should
            // never stash `Qunbound` in a hash table; this arm
            // exists only so the match stays exhaustive.
            ValueKind::Unbound | ValueKind::Unknown => HashKey::Ptr(self.bits()),
        }
    }

    fn to_eql_key(&self) -> HashKey {
        match self.kind() {
            ValueKind::Fixnum(n) => HashKey::Int(n),
            ValueKind::Float => HashKey::Float(self.xfloat().to_bits()),
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
            ValueKind::String => {
                // Use content for equal hashing
                if let Some(s) = self.as_str() {
                    HashKey::Text(s.to_string())
                } else {
                    self.to_eq_key()
                }
            }
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
                let key = closure_to_equal_key(*self, depth + 1, seen);
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
/// Characters are fixnums, so `(eq ?A 65)` is `t` (same bit pattern).
pub fn eq_value(left: &Value, right: &Value) -> bool {
    left.bits() == right.bits()
}

/// `eql` — like `eq` but also value-equality for numbers of same type.
pub fn eql_value(left: &Value, right: &Value) -> bool {
    if left.bits() == right.bits() {
        return true;
    }
    match (left.kind(), right.kind()) {
        (ValueKind::Float, ValueKind::Float) => left.xfloat().to_bits() == right.xfloat().to_bits(),
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
        (ValueKind::Float, ValueKind::Float) => left.xfloat().to_bits() == right.xfloat().to_bits(),
        (ValueKind::Symbol(a), ValueKind::Symbol(b)) => a == b,
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
        (
            ValueKind::Veclike(VecLikeType::HashTable),
            ValueKind::Veclike(VecLikeType::HashTable),
        ) => left.bits() == right.bits(),
        (ValueKind::Veclike(VecLikeType::Lambda), ValueKind::Veclike(VecLikeType::Lambda)) => {
            let pair = (left.bits(), right.bits());
            if !seen.insert(pair) {
                return true;
            }
            closure_equal(left, right, depth + 1, seen)
        }
        // For all other same-type veclike comparisons, use identity
        (ValueKind::Veclike(a), ValueKind::Veclike(b)) if a == b => left.bits() == right.bits(),
        _ => false,
    }
}

fn closure_params_to_equal_key(params: &LambdaParams) -> HashKey {
    let mut values = Vec::with_capacity(params.required.len() + params.optional.len() + 3);
    values.push(HashKey::Text("params".to_string()));
    for sym in &params.required {
        values.push(HashKey::Symbol(*sym));
    }
    if !params.optional.is_empty() {
        values.push(HashKey::Text("&optional".to_string()));
        for sym in &params.optional {
            values.push(HashKey::Symbol(*sym));
        }
    }
    if let Some(rest) = params.rest {
        values.push(HashKey::Text("&rest".to_string()));
        values.push(HashKey::Symbol(rest));
    }
    HashKey::EqualVec(values)
}

fn closure_to_equal_key(value: Value, depth: usize, seen: &mut Vec<usize>) -> HashKey {
    if depth > 200 {
        return HashKey::Text("#<lambda-depth-limit>".to_string());
    }

    let Some(params) = value.closure_params() else {
        return HashKey::Text("#<invalid-lambda>".to_string());
    };

    let mut slots = vec![
        HashKey::Text("lambda".to_string()),
        closure_params_to_equal_key(params),
        value
            .closure_body_value()
            .map_or(HashKey::Nil, |body| body.to_equal_key_depth(0, seen)),
        match value.closure_env().unwrap_or(None) {
            Some(env) => env.to_equal_key_depth(0, seen),
            None => HashKey::Text("dynamic".to_string()),
        },
    ];

    if let Some(doc_value) = value.closure_doc_value()
        && !doc_value.is_nil()
    {
        slots.push(HashKey::Nil);
        let doc = if doc_value.is_string() {
            let string = doc_value.as_lisp_string().expect("string");
            HashKey::Text(
                crate::emacs_core::string_escape::emacs_bytes_to_storage_string(
                    string.as_bytes(),
                    string.is_multibyte(),
                ),
            )
        } else {
            doc_value.to_equal_key_depth(0, seen)
        };
        slots.push(doc);
    }

    HashKey::EqualVec(slots)
}

fn closure_equal(
    left: &Value,
    right: &Value,
    depth: usize,
    seen: &mut HashSet<(usize, usize)>,
) -> bool {
    let (Some(left_params), Some(right_params)) = (left.closure_params(), right.closure_params())
    else {
        return false;
    };
    if left_params != right_params {
        return false;
    }

    let body_equal = match (left.closure_body_value(), right.closure_body_value()) {
        (Some(left_body), Some(right_body)) => {
            equal_value_inner(&left_body, &right_body, depth + 1, seen)
        }
        (None, None) => true,
        _ => false,
    };
    if !body_equal {
        return false;
    }

    let env_equal = match (
        left.closure_env().unwrap_or(None),
        right.closure_env().unwrap_or(None),
    ) {
        (None, None) => true,
        (Some(l), Some(r)) => equal_value_inner(&l, &r, depth + 1, seen),
        _ => false,
    };
    if !env_equal || left.closure_docstring().flatten() != right.closure_docstring().flatten() {
        return false;
    }

    match (
        left.closure_doc_form().flatten(),
        right.closure_doc_form().flatten(),
    ) {
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
        ValueKind::T => Some(SymId(1)),
        ValueKind::Nil => Some(SymId(0)),
        _ => None,
    }
}

fn lexenv_binding_symbol_value(sym_id: SymId) -> Value {
    TaggedValue::from_sym_id(sym_id)
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
// Test assertion helpers
// ---------------------------------------------------------------------------

/// Structural equality assertion for Values.
///
/// `PartialEq` on `TaggedValue` is bitwise (pointer identity for heap types),
/// matching GNU Emacs `eq` semantics. Tests that compare VALUES structurally
/// (like `assert_eq!(eval("(cons 1 2)"), Value::cons(...))`) must use this
/// macro instead of `assert_eq!`.
#[cfg(test)]
#[macro_export]
macro_rules! assert_val_eq {
    ($left:expr, $right:expr) => {{
        let left_val = &$left;
        let right_val = &$right;
        if !$crate::emacs_core::value::equal_value(left_val, right_val, 0) {
            panic!(
                "assertion `left == right` failed (structural)\n  left: {}\n right: {}",
                $crate::emacs_core::print::print_value(left_val),
                $crate::emacs_core::print::print_value(right_val),
            );
        }
    }};
    ($left:expr, $right:expr, $($msg:tt)+) => {{
        let left_val = &$left;
        let right_val = &$right;
        if !$crate::emacs_core::value::equal_value(left_val, right_val, 0) {
            panic!(
                "assertion `left == right` failed (structural): {}\n  left: {}\n right: {}",
                format_args!($($msg)+),
                $crate::emacs_core::print::print_value(left_val),
                $crate::emacs_core::print::print_value(right_val),
            );
        }
    }};
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "value_test.rs"]
mod tests;
