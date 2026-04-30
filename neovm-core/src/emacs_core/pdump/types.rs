//! Snapshot types for portable dump (pdump) serialization.
//!
//! These are rkyv-serializable mirrors of the runtime types in the evaluator.
//! Each `Dump*` type maps 1:1 to a runtime type but uses only plain data
//! (no Rc, HashMap, raw pointers, thread-locals).

use serde::{Deserialize, Serialize};

use crate::heap_types::LispString;

// ---------------------------------------------------------------------------
// Primitive identifiers
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpHeapRef {
    pub index: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpSymId(pub u32);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpNameId(pub u32);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpBufferId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpByteSpan {
    pub offset: u64,
    pub len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpSlotSpan {
    pub offset: u64,
    pub len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpConsSpan {
    pub offset: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpFloatSpan {
    pub offset: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpStringSpan {
    pub offset: u64,
    pub len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpVecLikeSpan {
    pub offset: u64,
    pub len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DumpByteData {
    Owned(Vec<u8>),
    Mapped(DumpByteSpan),
    StaticRoData { key: u64, len: u64 },
}

impl DumpByteData {
    pub fn owned(data: Vec<u8>) -> Self {
        Self::Owned(data)
    }

    pub fn mapped(offset: u64, len: u64) -> Self {
        Self::Mapped(DumpByteSpan { offset, len })
    }

    pub fn static_rodata(key: u64, len: u64) -> Self {
        Self::StaticRoData { key, len }
    }

    pub fn as_owned_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Owned(data) => Some(data),
            Self::Mapped(_) | Self::StaticRoData { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Value
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpValue {
    Nil,
    True,
    Int(i64),
    Float(DumpHeapRef),
    Symbol(DumpSymId),
    Str(DumpHeapRef),
    Cons(DumpHeapRef),
    Vector(DumpHeapRef),
    Record(DumpHeapRef),
    HashTable(DumpHeapRef),
    Lambda(DumpHeapRef),
    Macro(DumpHeapRef),
    Subr(DumpNameId),
    ByteCode(DumpHeapRef),
    Marker(DumpHeapRef),
    Overlay(DumpHeapRef),
    Buffer(DumpBufferId),
    Window(u64),
    Frame(u64),
    Timer(u64),
    /// Bignum serialized as a base-10 decimal string. We don't share
    /// bignums via heap refs because they're immutable and the dump
    /// format only needs to recreate the value, not its identity.
    Bignum(String),
    /// The `Qunbound` sentinel. Reaches the dump path only via a
    /// `local_var_alist` entry whose cdr marks a void per-buffer
    /// binding (mirrors GNU storing `(sym . Qunbound)` for
    /// `make-local-variable` on a void symbol, `data.c:2285-2289`).
    Unbound,
}

impl Default for DumpValue {
    fn default() -> Self {
        Self::Nil
    }
}

// ---------------------------------------------------------------------------
// Heap objects
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpHeapObject {
    Cons {
        car: DumpValue,
        cdr: DumpValue,
    },
    Vector(Vec<DumpValue>),
    HashTable(DumpLispHashTable),
    Str {
        data: DumpByteData,
        size: usize,
        size_byte: i64,
        #[serde(default)]
        text_props: Vec<DumpStringTextPropertyRun>,
    },
    Float(f64),
    Lambda(Vec<DumpValue>),
    Macro(Vec<DumpValue>),
    ByteCode(DumpByteCodeFunction),
    Record(Vec<DumpValue>),
    Marker(DumpMarker),
    Overlay(DumpOverlay),
    Buffer(DumpBufferId),
    Window(u64),
    Frame(u64),
    Timer(u64),
    Subr {
        name: DumpNameId,
        min_args: u16,
        max_args: Option<u16>,
    },
    Free,
}

// ---------------------------------------------------------------------------
// Lambda / ByteCode
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpLambdaParams {
    pub required: Vec<DumpSymId>,
    pub optional: Vec<DumpSymId>,
    pub rest: Option<DumpSymId>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpByteCodeFunction {
    pub ops: Vec<DumpOp>,
    pub constants: Vec<DumpValue>,
    pub max_stack: u16,
    pub params: DumpLambdaParams,
    #[serde(default)]
    pub arglist: Option<DumpValue>,
    #[serde(default)]
    pub lexical: bool,
    pub env: Option<DumpValue>,
    pub gnu_byte_offset_map: Option<Vec<(u32, u32)>>,
    #[serde(default)]
    pub gnu_bytecode_bytes: Option<Vec<u8>>,
    pub docstring: Option<DumpLispString>,
    pub doc_form: Option<DumpValue>,
    #[serde(default)]
    pub interactive: Option<DumpValue>,
    #[serde(default)]
    pub closure_slot_count: usize,
    #[serde(default)]
    pub extra_slots: Vec<DumpValue>,
}

// ---------------------------------------------------------------------------
// Bytecode opcodes
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpOp {
    Constant(u16),
    Nil,
    True,
    Pop,
    Dup,
    StackRef(u16),
    StackSet(u16),
    DiscardN(u8),
    VarRef(u16),
    VarSet(u16),
    VarBind(u16),
    Unbind(u16),
    Call(u16),
    Apply(u16),
    Goto(u32),
    GotoIfNil(u32),
    GotoIfNotNil(u32),
    GotoIfNilElsePop(u32),
    GotoIfNotNilElsePop(u32),
    Switch,
    Return,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Add1,
    Sub1,
    Negate,
    Eqlsign,
    Gtr,
    Lss,
    Leq,
    Geq,
    Max,
    Min,
    Car,
    Cdr,
    Cons,
    List(u16),
    Length,
    Nth,
    Nthcdr,
    Setcar,
    Setcdr,
    CarSafe,
    CdrSafe,
    Elt,
    Nconc,
    Nreverse,
    Member,
    Memq,
    Assq,
    Symbolp,
    Consp,
    Stringp,
    Listp,
    Integerp,
    Numberp,
    Null,
    Not,
    Eq,
    Equal,
    Concat(u16),
    Substring,
    StringEqual,
    StringLessp,
    Aref,
    Aset,
    SymbolValue,
    SymbolFunction,
    Set,
    Fset,
    Get,
    Put,
    PushConditionCase(u32),
    PushConditionCaseRaw(u32),
    PushCatch(u32),
    PopHandler,
    UnwindProtect(u32),
    UnwindProtectPop,
    Throw,
    SaveCurrentBuffer,
    SaveExcursion,
    SaveRestriction,
    SaveWindowExcursion,
    MakeClosure(u16),
    CallBuiltin(u16, u8),
    /// Built-in dispatch by direct symbol name (GNU inline-dispatch
    /// mirror). Serialized as the symbol's name so SymId mapping
    /// survives across dump/load.
    CallBuiltinSym(DumpSymId, u8),
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Hash tables
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpHashTableTest {
    Eq,
    Eql,
    Equal,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpHashTableWeakness {
    Key,
    Value,
    KeyOrValue,
    KeyAndValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpHashKey {
    Nil,
    True,
    Int(i64),
    Float(u64),
    FloatEq(u64, u32),
    Symbol(DumpSymId),
    Keyword(DumpSymId),
    Str(DumpHeapRef),
    Char(char),
    Window(u64),
    Frame(u64),
    Ptr(u64),
    HeapRef(u32),
    EqualCons(Box<DumpHashKey>, Box<DumpHashKey>),
    EqualVec(Vec<DumpHashKey>),
    SymbolWithPos(Box<DumpHashKey>, Box<DumpHashKey>),
    Cycle(u32),
    Text(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpLispHashTable {
    pub test: DumpHashTableTest,
    pub test_name: Option<DumpSymId>,
    pub size: i64,
    pub weakness: Option<DumpHashTableWeakness>,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub entries: Vec<(DumpHashKey, DumpValue)>,
    pub key_snapshots: Vec<(DumpHashKey, DumpValue)>,
    pub insertion_order: Vec<DumpHashKey>,
}

// ---------------------------------------------------------------------------
// Symbols / Obarray
// ---------------------------------------------------------------------------

/// Serialized value cell for a symbol.  Replaces the old
/// `DumpSymbolValue` (which wrapped `Option<DumpValue>` for the plain case
/// and had separate legacy `value`/`special`/`constant` fields).
///
/// Added in pdump format v21 alongside the removal of the `SymbolValue`
/// enum from `LispSymbol`.  The variant tag directly mirrors the
/// `SymbolRedirect` discriminant stored in `SymbolFlags`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpSymbolVal {
    /// `SymbolRedirect::Plainval` — value is in `val.plain`.
    /// `DumpValue::Unbound` encodes the unbound sentinel.
    Plain(DumpValue),
    /// `SymbolRedirect::Varalias` — value cell aliases another symbol.
    Alias(DumpSymId),
    /// `SymbolRedirect::Localized` — buffer-local variable with a BLV.
    /// `default` is the global default value (the `defcell` cdr).
    /// `local_if_set` mirrors `LispBufferLocalValue::local_if_set`.
    Localized {
        default: DumpValue,
        local_if_set: bool,
    },
    /// `SymbolRedirect::Forwarded` — forwarded to a Rust-side variable.
    /// These are re-installed from `BUFFER_SLOT_INFO` at load time, so
    /// the dump only needs to signal "this symbol is a forwarder"; the
    /// actual descriptor pointer is never serialized.
    Forwarded,
}

/// Serialized per-symbol metadata.  Format v21: all legacy fields
/// (`name`, `value`, `symbol_value`, `special`, `constant`) are removed;
/// the value cell is encoded directly as a `DumpSymbolVal` variant, and
/// the flag byte fields (`redirect`, `trapped_write`, `interned`,
/// `declared_special`) mirror `SymbolFlags` exactly.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSymbolData {
    /// Redirect tag: 0=Plainval, 1=Varalias, 2=Localized, 3=Forwarded.
    /// Redundant with `val`'s variant tag but kept for clarity and to
    /// allow future validation on load.
    pub redirect: u8,
    /// Trapped-write tag: 0=Untrapped, 1=NoWrite, 2=Trapped.
    pub trapped_write: u8,
    /// Interned tag: 0=Uninterned, 1=Interned, 2=InternedInInitial.
    pub interned: u8,
    /// `declared_special` flag (mirrors `SymbolFlags::declared_special`).
    pub declared_special: bool,
    /// The value cell, encoded as a `DumpSymbolVal` variant.
    pub val: DumpSymbolVal,
    /// Function slot. `DumpValue::Nil` is the unbound sentinel.
    pub function: DumpValue,
    /// Property list as a Lisp cons list (DumpValue::Nil = empty).
    pub plist: DumpValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpObarray {
    pub symbols: Vec<(DumpSymId, DumpSymbolData)>,
    pub global_members: Vec<DumpSymId>,
    pub function_unbound: Vec<DumpSymId>,
    pub function_epoch: u64,
}

// ---------------------------------------------------------------------------
// Dump-wide symbol table
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSymbolEntry {
    /// Dump-local name atom id for this symbol slot.
    pub name: DumpNameId,
    /// `true` when the corresponding symbol id is canonical/interned and
    /// `false` for uninterned symbols created via `make-symbol`.
    pub canonical: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSymbolTable {
    /// Dump-local symbol-name atoms. Multiple symbols may point at the same
    /// `DumpNameId` when they share a print name.
    pub names: Vec<LispString>,
    /// One entry per dump-local symbol id.
    pub symbols: Vec<DumpSymbolEntry>,
}

// ---------------------------------------------------------------------------
// Tagged heap snapshot
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpTaggedHeap {
    pub objects: Vec<DumpHeapObject>,
    #[serde(default)]
    pub mapped_cons: Vec<Option<DumpConsSpan>>,
    #[serde(default)]
    pub mapped_floats: Vec<Option<DumpFloatSpan>>,
    #[serde(default)]
    pub mapped_strings: Vec<Option<DumpStringSpan>>,
    #[serde(default)]
    pub mapped_veclikes: Vec<Option<DumpVecLikeSpan>>,
    #[serde(default)]
    pub mapped_slots: Vec<Option<DumpSlotSpan>>,
}

// ---------------------------------------------------------------------------
// OrderedSymMap (dynamic binding frame)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpRuntimeBindingValue {
    Bound(DumpValue),
    Void,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpOrderedSymMap {
    pub entries: Vec<(DumpSymId, DumpRuntimeBindingValue)>,
}

// ---------------------------------------------------------------------------
// String text properties
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpStringTextPropertyRun {
    pub start: usize,
    pub end: usize,
    pub plist: DumpValue,
}

// ---------------------------------------------------------------------------
// Buffer types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpGapBuffer {
    pub text: Vec<u8>,
}

// `DumpInsertionType` was retired in v26 alongside `DumpMarkerEntry`:
// the marker chain now serializes through `DumpMarker`, which encodes
// the insertion type as a plain `bool` matching `MarkerData`.

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpPropertyInterval {
    pub start: usize,
    pub end: usize,
    pub properties: Vec<(DumpValue, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpTextPropertyTable {
    pub intervals: Vec<DumpPropertyInterval>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpOverlay {
    pub plist: DumpValue,
    pub buffer: Option<DumpBufferId>,
    pub start: usize,
    pub end: usize,
    pub front_advance: bool,
    pub rear_advance: bool,
}

/// Pdump v26: marker shape mirrors `MarkerData` post-GNU-parity refactor.
///
/// The legacy `position: Option<i64>` cache is gone — `bytepos` and `charpos`
/// are the authoritative on-disk fields, matching the runtime `MarkerData`
/// layout. Used both for individual heap-object marker decode
/// (`DumpHeapObject::Marker`) and for `DumpBuffer.markers` chain entries.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMarker {
    pub buffer: Option<DumpBufferId>,
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
    pub bytepos: usize,
    pub charpos: usize,
    /// Mirror of `MarkerData.last_position_valid`. Defaulted for back-compat
    /// with pre-parity dumps; older dumps come back as `false` and a single
    /// re-set will repopulate the flag.
    #[serde(default)]
    pub last_position_valid: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpOverlayList {
    pub overlays: Vec<DumpOverlay>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpUndoRecord {
    Insert {
        pos: usize,
        len: usize,
    },
    Delete {
        pos: usize,
        text: String,
    },
    PropertyChange {
        pos: usize,
        len: usize,
        old_props: Vec<(String, DumpValue)>,
    },
    CursorMove {
        pos: usize,
    },
    FirstChange {
        visited_file_modtime: i64,
    },
    Boundary,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpUndoList {
    pub records: Vec<DumpUndoRecord>,
    pub limit: usize,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBuffer {
    pub id: DumpBufferId,
    #[serde(default)]
    pub name_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub name: Option<String>,
    pub base_buffer: Option<DumpBufferId>,
    pub text: DumpGapBuffer,
    pub pt: usize,
    #[serde(default)]
    pub pt_char: Option<usize>,
    pub mark: Option<usize>,
    #[serde(default)]
    pub mark_char: Option<usize>,
    pub begv: usize,
    #[serde(default)]
    pub begv_char: Option<usize>,
    pub zv: usize,
    #[serde(default)]
    pub zv_char: Option<usize>,
    pub modified: bool,
    pub modified_tick: i64,
    pub chars_modified_tick: i64,
    #[serde(default)]
    pub save_modified_tick: Option<i64>,
    #[serde(default)]
    pub autosave_modified_tick: Option<i64>,
    #[serde(default)]
    pub last_window_start: Option<usize>,
    pub read_only: bool,
    pub multibyte: bool,
    #[serde(default)]
    pub file_name_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub auto_save_file_name_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub auto_save_file_name: Option<String>,
    /// v26: chain order, head→tail. Each entry is a full `DumpMarker`
    /// (the same shape used by `DumpHeapObject::Marker`); the load-side
    /// chain reconstruction reuses the heap-allocated MarkerObj for the
    /// same `marker_id` to preserve identity with Lisp references.
    pub markers: Vec<DumpMarker>,
    #[serde(default)]
    pub state_pt_marker: Option<u64>,
    #[serde(default)]
    pub state_begv_marker: Option<u64>,
    #[serde(default)]
    pub state_zv_marker: Option<u64>,
    #[serde(default)]
    pub properties_syms: Vec<(DumpSymId, DumpRuntimeBindingValue)>,
    pub properties: Vec<(String, DumpRuntimeBindingValue)>,
    #[serde(default)]
    pub local_binding_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub local_binding_names: Vec<String>,
    #[serde(default)]
    pub local_map: DumpValue,
    pub text_props: DumpTextPropertyTable,
    pub overlays: DumpOverlayList,
    /// Legacy field — retained for backward compatibility with old pdump files.
    /// New dumps always write an empty DumpUndoList here; the real undo state
    /// lives inside the `properties` map as `buffer-undo-list`.
    #[serde(default)]
    pub undo_list: Option<DumpUndoList>,
    /// Phase 11: BUFFER_OBJFWD slot table values. One DumpValue per
    /// `Buffer::slots[]` entry, in offset order. Empty for legacy
    /// (pre-format-11) dumps; load_buffer falls back to seeding
    /// from BUFFER_SLOT_INFO defaults + the legacy file_name etc.
    /// fields when this is empty.
    #[serde(default)]
    pub slots: Vec<DumpValue>,
    /// Phase 11: per-slot "is buffer-local in this buffer" bitmap.
    /// Mirrors `Buffer::local_flags` (Phase 10D). Defaults to 0
    /// for legacy dumps.
    #[serde(default)]
    pub local_flags: u64,
    /// Phase 11: `local_var_alist` for SYMBOL_LOCALIZED variables.
    /// Defaults to `Nil` for legacy dumps.
    #[serde(default)]
    pub local_var_alist: DumpValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBufferManager {
    pub buffers: Vec<(DumpBufferId, DumpBuffer)>,
    pub current: Option<DumpBufferId>,
    pub next_id: u64,
    pub next_marker_id: u64,
    /// Runtime `buffer-defaults` slot table — one DumpValue per
    /// `BufferManager::buffer_defaults[]` entry in offset order.
    /// Mirrors GNU's static `struct buffer buffer_defaults` that
    /// pdump preserves as part of the dumped image.
    ///
    /// Bindings.el's `setq-default mode-line-format <rich-list>` at
    /// load time mutates this table; before this field was added to
    /// the dump schema, the mutation was lost on pdump-load and the
    /// layout engine saw the install-time `"%-"` seed. See
    /// `project_modeline_buffer_defaults_dump.md`.
    ///
    /// `#[serde(default)]` so older pdumps (no field present) keep
    /// loading and fall back to the install-time seeds via
    /// `BUFFER_SLOT_INFO` in `load_buffer_manager`.
    #[serde(default)]
    pub buffer_defaults: Vec<DumpValue>,
}

// ---------------------------------------------------------------------------
// Sub-manager types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpLispString {
    pub data: Vec<u8>,
    pub size: usize,
    pub size_byte: i64,
}

// Autoload
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpAutoloadType {
    Function,
    Macro,
    Keymap,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAutoloadEntry {
    pub file: DumpLispString,
    pub docstring: Option<DumpLispString>,
    pub interactive: bool,
    pub autoload_type: DumpAutoloadType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAutoloadManager {
    #[serde(default)]
    pub entries_syms: Vec<(DumpSymId, DumpAutoloadEntry)>,
    pub entries: Vec<(String, DumpAutoloadEntry)>,
    #[serde(default)]
    pub after_load_lisp: Vec<(DumpLispString, Vec<DumpValue>)>,
    #[serde(default)]
    pub after_load: Vec<(String, Vec<DumpValue>)>,
    pub loaded_files: Vec<DumpLispString>,
    #[serde(default)]
    pub obsolete_functions_syms: Vec<(DumpSymId, (DumpLispString, DumpLispString))>,
    pub obsolete_functions: Vec<(String, (String, String))>,
    #[serde(default)]
    pub obsolete_variables_syms: Vec<(DumpSymId, (DumpLispString, DumpLispString))>,
    pub obsolete_variables: Vec<(String, (String, String))>,
}

// Custom
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCustomManager {
    #[serde(default)]
    pub auto_buffer_local_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub auto_buffer_local: Vec<String>,
}

// Abbrev
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrev {
    pub expansion: DumpLispString,
    pub hook: Option<DumpLispString>,
    pub count: usize,
    pub system: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrevTable {
    pub name: DumpLispString,
    pub abbrevs: Vec<(DumpLispString, DumpAbbrev)>,
    pub parent: Option<DumpLispString>,
    pub case_fixed: bool,
    pub enable_quoting: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrevManager {
    #[serde(default)]
    pub tables_syms: Vec<(DumpSymId, DumpAbbrevTable)>,
    #[serde(default)]
    pub tables: Vec<(String, DumpAbbrevTable)>,
    #[serde(default)]
    pub global_table_sym: Option<DumpSymId>,
    pub global_table_name: DumpLispString,
    pub abbrev_mode: bool,
}

// Interactive
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpInteractiveSpec {
    pub spec: DumpValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpInteractiveRegistry {
    pub specs: Vec<(DumpSymId, DumpInteractiveSpec)>,
}

// Mode
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontLockKeyword {
    #[serde(default)]
    pub pattern_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub face_sym: Option<DumpSymId>,
    #[serde(default)]
    pub face: Option<String>,
    pub group: usize,
    pub override_: bool,
    pub laxmatch: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontLockDefaults {
    pub keywords: Vec<DumpFontLockKeyword>,
    pub case_fold: bool,
    #[serde(default)]
    pub syntax_table_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub syntax_table: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMajorMode {
    pub pretty_name: DumpLispString,
    pub parent: Option<DumpValue>,
    pub mode_hook: DumpValue,
    pub keymap_name: Option<DumpValue>,
    pub syntax_table_name: Option<DumpValue>,
    pub abbrev_table_name: Option<DumpValue>,
    pub font_lock: Option<DumpFontLockDefaults>,
    pub body: Option<DumpValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMinorMode {
    pub lighter: Option<DumpLispString>,
    pub keymap_name: Option<DumpValue>,
    pub global: bool,
    pub body: Option<DumpValue>,
}

// mode.rs has its own CustomVariable/CustomGroup — we mirror those separately
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpModeCustomVariable {
    pub default_value: DumpValue,
    pub doc: Option<DumpLispString>,
    pub custom_type: DumpModeCustomType,
    pub group: Option<DumpValue>,
    pub set_function: Option<DumpValue>,
    pub get_function: Option<DumpValue>,
    pub tag: Option<DumpLispString>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpModeCustomType {
    Boolean,
    Integer,
    Float,
    String,
    Symbol,
    Sexp,
    Choice(Vec<(String, DumpValue)>),
    List(Box<DumpModeCustomType>),
    Alist(Box<DumpModeCustomType>, Box<DumpModeCustomType>),
    Plist(Box<DumpModeCustomType>, Box<DumpModeCustomType>),
    Color,
    Face,
    File,
    Directory,
    Function,
    Variable,
    Hook,
    Coding,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpModeCustomGroup {
    pub doc: Option<DumpLispString>,
    pub parent: Option<DumpValue>,
    pub members: Vec<DumpValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpModeRegistry {
    pub major_modes: Vec<(DumpSymId, DumpMajorMode)>,
    pub minor_modes: Vec<(DumpSymId, DumpMinorMode)>,
    pub buffer_major_modes: Vec<(u64, DumpValue)>,
    pub buffer_minor_modes: Vec<(u64, Vec<DumpValue>)>,
    pub global_minor_modes: Vec<DumpValue>,
    #[serde(default)]
    pub auto_mode_alist_lisp: Vec<(DumpLispString, DumpValue)>,
    #[serde(default)]
    pub auto_mode_alist: Vec<(String, DumpValue)>,
    pub custom_variables: Vec<(DumpSymId, DumpModeCustomVariable)>,
    pub custom_groups: Vec<(DumpSymId, DumpModeCustomGroup)>,
    pub fundamental_mode: DumpValue,
}

// Coding
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpEolType {
    Unix,
    Dos,
    Mac,
    Undecided,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCodingSystemInfo {
    #[serde(default)]
    pub name_sym: Option<DumpSymId>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub coding_type_sym: Option<DumpSymId>,
    #[serde(default)]
    pub coding_type: Option<String>,
    pub mnemonic: char,
    pub eol_type: DumpEolType,
    pub ascii_compatible_p: bool,
    #[serde(default)]
    pub charset_list_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub charset_list: Vec<String>,
    #[serde(default)]
    pub post_read_conversion_sym: Option<DumpSymId>,
    #[serde(default)]
    pub post_read_conversion: Option<String>,
    #[serde(default)]
    pub pre_write_conversion_sym: Option<DumpSymId>,
    #[serde(default)]
    pub pre_write_conversion: Option<String>,
    pub default_char: Option<char>,
    pub for_unibyte: bool,
    #[serde(default)]
    pub properties_syms: Vec<(DumpSymId, DumpValue)>,
    #[serde(default)]
    pub properties: Vec<(String, DumpValue)>,
    pub int_properties: Vec<(i64, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCodingSystemManager {
    #[serde(default)]
    pub systems_syms: Vec<(DumpSymId, DumpCodingSystemInfo)>,
    #[serde(default)]
    pub systems: Vec<(String, DumpCodingSystemInfo)>,
    #[serde(default)]
    pub aliases_syms: Vec<(DumpSymId, DumpSymId)>,
    #[serde(default)]
    pub aliases: Vec<(String, String)>,
    #[serde(default)]
    pub priority_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub priority: Vec<String>,
    #[serde(default)]
    pub keyboard_coding_sym: Option<DumpSymId>,
    #[serde(default)]
    pub keyboard_coding: Option<String>,
    #[serde(default)]
    pub terminal_coding_sym: Option<DumpSymId>,
    #[serde(default)]
    pub terminal_coding: Option<String>,
}

// Charset
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetSubsetSpec {
    #[serde(default)]
    pub parent_sym: Option<DumpSymId>,
    #[serde(default)]
    pub parent: Option<String>,
    pub parent_min_code: i64,
    pub parent_max_code: i64,
    pub offset: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpCharsetMethod {
    Offset(i64),
    Map(String),
    Subset(DumpCharsetSubsetSpec),
    SupersetSyms(Vec<(DumpSymId, i64)>),
    Superset(Vec<(String, i64)>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetInfo {
    pub id: i64,
    #[serde(default)]
    pub name_sym: Option<DumpSymId>,
    #[serde(default)]
    pub name: Option<String>,
    pub dimension: i64,
    pub code_space: [i64; 8],
    pub min_code: i64,
    pub max_code: i64,
    pub iso_final_char: Option<i64>,
    pub iso_revision: Option<i64>,
    pub emacs_mule_id: Option<i64>,
    pub ascii_compatible_p: bool,
    pub supplementary_p: bool,
    pub invalid_code: Option<i64>,
    pub unify_map: DumpValue,
    pub method: DumpCharsetMethod,
    #[serde(default)]
    pub plist_syms: Vec<(DumpSymId, DumpValue)>,
    #[serde(default)]
    pub plist: Vec<(String, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetRegistry {
    pub charsets: Vec<DumpCharsetInfo>,
    #[serde(default)]
    pub priority_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub priority: Vec<String>,
    pub next_id: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpFontWidth {
    UltraCondensed,
    ExtraCondensed,
    Condensed,
    SemiCondensed,
    Normal,
    SemiExpanded,
    Expanded,
    ExtraExpanded,
    UltraExpanded,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpFontRepertory {
    Charset(String),
    CharTableRanges(Vec<(u32, u32)>),
    CharsetSym(DumpSymId),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpStoredFontSpec {
    #[serde(default)]
    pub family_sym: Option<DumpSymId>,
    pub family: Option<String>,
    #[serde(default)]
    pub registry_sym: Option<DumpSymId>,
    pub registry: Option<String>,
    #[serde(default)]
    pub lang_sym: Option<DumpSymId>,
    pub lang: Option<String>,
    pub weight: Option<u16>,
    pub slant: Option<DumpFontSlant>,
    pub width: Option<DumpFontWidth>,
    pub repertory: Option<DumpFontRepertory>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpFontSpecEntry {
    Font(DumpStoredFontSpec),
    ExplicitNone,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontsetRangeEntry {
    pub from: u32,
    pub to: u32,
    pub entries: Vec<DumpFontSpecEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontsetData {
    pub ranges: Vec<DumpFontsetRangeEntry>,
    pub fallback: Option<Vec<DumpFontSpecEntry>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontsetRegistry {
    #[serde(default)]
    pub ordered_names_lisp: Vec<DumpLispString>,
    #[serde(default)]
    pub alias_to_name_lisp: Vec<(DumpLispString, DumpLispString)>,
    #[serde(default)]
    pub fontsets_lisp: Vec<(DumpLispString, DumpFontsetData)>,
    #[serde(default)]
    pub ordered_names: Vec<String>,
    #[serde(default)]
    pub alias_to_name: Vec<(String, String)>,
    #[serde(default)]
    pub fontsets: Vec<(String, DumpFontsetData)>,
    pub generation: u64,
}

// Face
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct DumpColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpFontSlant {
    Normal,
    Italic,
    Oblique,
    ReverseItalic,
    ReverseOblique,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpUnderlineStyle {
    Line,
    Wave,
    Dot,
    Dash,
    DoubleLine,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpUnderline {
    pub style: DumpUnderlineStyle,
    pub color: Option<DumpColor>,
    pub position: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpBoxStyle {
    Flat,
    Raised,
    Pressed,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBoxBorder {
    pub color: Option<DumpColor>,
    pub width: i32,
    pub style: DumpBoxStyle,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpFaceHeight {
    Absolute(i32),
    Relative(f64),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFace {
    pub foreground: Option<DumpColor>,
    pub background: Option<DumpColor>,
    #[serde(default)]
    pub family_value: Option<DumpValue>,
    pub family: Option<String>,
    #[serde(default)]
    pub foundry_value: Option<DumpValue>,
    pub foundry: Option<String>,
    pub height: Option<DumpFaceHeight>,
    pub weight: Option<u16>,
    pub slant: Option<DumpFontSlant>,
    pub underline: Option<DumpUnderline>,
    pub overline: Option<bool>,
    pub strike_through: Option<bool>,
    pub box_border: Option<DumpBoxBorder>,
    pub inverse_video: Option<bool>,
    #[serde(default)]
    pub stipple_value: Option<DumpValue>,
    pub stipple: Option<String>,
    pub extend: Option<bool>,
    #[serde(default)]
    pub inherit_syms: Vec<DumpSymId>,
    #[serde(default)]
    pub inherit: Vec<String>,
    pub overstrike: bool,
    #[serde(default)]
    pub doc_value: Option<DumpValue>,
    pub doc: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFaceTable {
    #[serde(default)]
    pub face_ids: Vec<(DumpSymId, DumpFace)>,
    #[serde(default)]
    pub faces: Vec<(String, DumpFace)>,
}

// Rectangle
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpRectangleState {
    pub killed: Vec<DumpLispString>,
}

// Kmacro
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpKmacroManager {
    pub current_macro: Vec<DumpValue>,
    pub last_macro: Option<Vec<DumpValue>>,
    pub macro_ring: Vec<Vec<DumpValue>>,
    pub counter: i64,
    #[serde(default)]
    pub counter_format_lisp: Option<DumpLispString>,
    #[serde(default)]
    pub counter_format: Option<String>,
}

// Register
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpRegisterContent {
    Text {
        data: Vec<u8>,
        size: usize,
        size_byte: i64,
    },
    Number(i64),
    Marker(DumpValue),
    Rectangle(Vec<DumpLispString>),
    FrameConfig(DumpValue),
    File(DumpLispString),
    KbdMacro(Vec<DumpValue>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpRegisterManager {
    pub registers: Vec<(char, DumpRegisterContent)>,
}

// Bookmark
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBookmark {
    pub name: DumpLispString,
    pub filename: Option<String>,
    pub position: usize,
    pub front_context: Option<String>,
    pub rear_context: Option<String>,
    pub annotation: Option<String>,
    pub handler: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBookmarkManager {
    #[serde(default)]
    pub bookmarks_lisp: Vec<(DumpLispString, DumpBookmark)>,
    #[serde(default)]
    pub bookmarks: Vec<(String, DumpBookmark)>,
    pub recent: Vec<DumpLispString>,
}

// Variable watchers
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpVariableWatcherList {
    pub watchers: Vec<(DumpSymId, Vec<DumpValue>)>,
}

// ---------------------------------------------------------------------------
// Top-level evaluator state
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpContextState {
    pub symbol_table: DumpSymbolTable,
    pub tagged_heap: DumpTaggedHeap,
    pub obarray: DumpObarray,
    pub dynamic: Vec<DumpOrderedSymMap>,
    pub lexenv: DumpValue,
    pub features: Vec<DumpSymId>,
    pub require_stack: Vec<DumpSymId>,
    pub loads_in_progress: Vec<DumpLispString>,
    pub buffers: DumpBufferManager,
    pub autoloads: DumpAutoloadManager,
    pub custom: DumpCustomManager,
    pub modes: DumpModeRegistry,
    pub coding_systems: DumpCodingSystemManager,
    pub charset_registry: DumpCharsetRegistry,
    pub fontset_registry: DumpFontsetRegistry,
    pub face_table: DumpFaceTable,
    pub abbrevs: DumpAbbrevManager,
    pub interactive: DumpInteractiveRegistry,
    pub rectangle: DumpRectangleState,
    pub standard_syntax_table: DumpValue,
    pub standard_category_table: DumpValue,
    pub current_local_map: DumpValue,
    pub kmacro: DumpKmacroManager,
    pub registers: DumpRegisterManager,
    pub bookmarks: DumpBookmarkManager,
    pub watchers: DumpVariableWatcherList,
}
