//! Snapshot types for portable dump (pdump) serialization.
//!
//! These are rkyv-serializable mirrors of the runtime types in the evaluator.
//! Each `Dump*` type maps 1:1 to a runtime type but uses only plain data
//! (no Rc, HashMap, raw pointers, thread-locals).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Primitive identifiers
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpObjId {
    pub index: u32,
    pub generation: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpSymId(pub u32);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DumpBufferId(pub u64);

// ---------------------------------------------------------------------------
// Value
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpValue {
    Nil,
    True,
    Int(i64),
    Float(f64, u32),
    Symbol(DumpSymId),
    Keyword(DumpSymId),
    Str(DumpObjId),
    Cons(DumpObjId),
    Vector(DumpObjId),
    Record(DumpObjId),
    HashTable(DumpObjId),
    Lambda(DumpObjId),
    Macro(DumpObjId),
    Char(char),
    Subr(DumpSymId),
    ByteCode(DumpObjId),
    Buffer(DumpBufferId),
    Window(u64),
    Frame(u64),
    Timer(u64),
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
    Cons { car: DumpValue, cdr: DumpValue },
    Vector(Vec<DumpValue>),
    HashTable(DumpLispHashTable),
    Str { text: String, multibyte: bool },
    Lambda(DumpLambdaData),
    Macro(DumpLambdaData),
    ByteCode(DumpByteCodeFunction),
    Free,
}

// ---------------------------------------------------------------------------
// Lambda / ByteCode
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpLambdaData {
    pub params: DumpLambdaParams,
    pub body: Vec<DumpExpr>,
    pub env: Option<DumpValue>,
    pub docstring: Option<String>,
    pub doc_form: Option<DumpValue>,
}

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
    pub lexical: bool,
    pub env: Option<DumpValue>,
    pub gnu_byte_offset_map: Option<Vec<(u32, u32)>>,
    pub docstring: Option<String>,
    pub doc_form: Option<DumpValue>,
    #[serde(default)]
    pub interactive: Option<DumpValue>,
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
    MakeClosure(u16),
    CallBuiltin(u16, u8),
}

// ---------------------------------------------------------------------------
// Expressions (code AST)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpExpr {
    Int(i64),
    Float(f64),
    Symbol(DumpSymId),
    ReaderLoadFileName,
    Keyword(DumpSymId),
    Str(String),
    Char(char),
    List(Vec<DumpExpr>),
    Vector(Vec<DumpExpr>),
    DottedList(Vec<DumpExpr>, Box<DumpExpr>),
    Bool(bool),
    OpaqueValue(DumpValue),
}

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
    Str(DumpObjId),
    Char(char),
    Window(u64),
    Frame(u64),
    Ptr(u64),
    ObjId(u32, u32),
    EqualCons(Box<DumpHashKey>, Box<DumpHashKey>),
    EqualVec(Vec<DumpHashKey>),
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

/// Serializable representation of [`crate::emacs_core::symbol::SymbolValue`].
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpSymbolValue {
    /// Plain value (GNU: SYMBOL_PLAINVAL).
    Plain(Option<DumpValue>),
    /// Alias to another symbol (GNU: SYMBOL_VARALIAS).
    Alias(DumpSymId),
    /// Buffer-local variable (GNU: SYMBOL_LOCALIZED).
    BufferLocal {
        default: Option<DumpValue>,
        local_if_set: bool,
    },
    /// Forwarded to Rust variable (GNU: SYMBOL_FORWARDED) — placeholder.
    Forwarded,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSymbolData {
    pub name: DumpSymId,
    /// The symbol value cell.  Older dumps may still have the `value` field
    /// (kept for backward compatibility via `#[serde(default)]`).
    #[serde(default)]
    pub value: Option<DumpValue>,
    /// New enum-based value cell.  Present in dumps produced after the
    /// `SymbolValue` refactor.
    #[serde(default)]
    pub symbol_value: Option<DumpSymbolValue>,
    pub function: Option<DumpValue>,
    pub plist: Vec<(DumpSymId, DumpValue)>,
    pub special: bool,
    pub constant: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpObarray {
    pub symbols: Vec<(u32, DumpSymbolData)>,
    pub global_members: Vec<u32>,
    pub function_unbound: Vec<u32>,
    pub function_epoch: u64,
}

// ---------------------------------------------------------------------------
// String interner
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpStringInterner {
    pub strings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpLispHeap {
    pub objects: Vec<DumpHeapObject>,
    pub generations: Vec<u32>,
    pub free_list: Vec<u32>,
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpInsertionType {
    Before,
    After,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMarkerEntry {
    pub id: u64,
    pub byte_pos: usize,
    #[serde(default)]
    pub char_pos: Option<usize>,
    pub insertion_type: DumpInsertionType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpPropertyInterval {
    pub start: usize,
    pub end: usize,
    pub properties: Vec<(String, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpTextPropertyTable {
    pub intervals: Vec<DumpPropertyInterval>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpOverlay {
    pub id: u64,
    pub start: usize,
    pub end: usize,
    pub properties: Vec<(String, DumpValue)>,
    pub front_advance: bool,
    pub rear_advance: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpOverlayList {
    pub overlays: Vec<DumpOverlay>,
    pub next_id: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum DumpSyntaxClass {
    Whitespace,
    Word,
    Symbol,
    Punctuation,
    Open,
    Close,
    Prefix,
    StringDelim,
    MathDelim,
    Escape,
    CharQuote,
    Comment,
    EndComment,
    InheritStandard,
    Generic,
    StringFence,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSyntaxEntry {
    pub class: DumpSyntaxClass,
    pub matching_char: Option<char>,
    pub flags: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpSyntaxTable {
    pub entries: Vec<(char, DumpSyntaxEntry)>,
    pub parent: Option<Box<DumpSyntaxTable>>,
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
    pub name: String,
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
    pub read_only: bool,
    pub multibyte: bool,
    pub file_name: Option<String>,
    #[serde(default)]
    pub auto_save_file_name: Option<String>,
    pub markers: Vec<DumpMarkerEntry>,
    pub properties: Vec<(String, DumpRuntimeBindingValue)>,
    #[serde(default)]
    pub local_map: DumpValue,
    pub text_props: DumpTextPropertyTable,
    pub overlays: DumpOverlayList,
    pub syntax_table: DumpSyntaxTable,
    /// Legacy field — retained for backward compatibility with old pdump files.
    /// New dumps always write an empty DumpUndoList here; the real undo state
    /// lives inside the `properties` map as `buffer-undo-list`.
    #[serde(default)]
    pub undo_list: Option<DumpUndoList>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBufferManager {
    pub buffers: Vec<(DumpBufferId, DumpBuffer)>,
    pub current: Option<DumpBufferId>,
    pub next_id: u64,
    pub next_marker_id: u64,
}

// ---------------------------------------------------------------------------
// Sub-manager types
// ---------------------------------------------------------------------------

// Autoload
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpAutoloadType {
    Function,
    Macro,
    Keymap,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAutoloadEntry {
    pub name: String,
    pub file: String,
    pub docstring: Option<String>,
    pub interactive: bool,
    pub autoload_type: DumpAutoloadType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAutoloadManager {
    pub entries: Vec<(String, DumpAutoloadEntry)>,
    pub after_load: Vec<(String, Vec<DumpValue>)>,
    pub loaded_files: Vec<String>,
    pub obsolete_functions: Vec<(String, (String, String))>,
    pub obsolete_variables: Vec<(String, (String, String))>,
}

// Custom
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCustomVariable {
    pub name: String,
    pub custom_type: DumpValue,
    pub group: Option<String>,
    pub documentation: Option<String>,
    pub standard_value: DumpValue,
    pub set_function: Option<DumpValue>,
    pub get_function: Option<DumpValue>,
    pub initialize: Option<DumpValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCustomGroup {
    pub name: String,
    pub members: Vec<(String, DumpValue)>,
    pub documentation: Option<String>,
    pub parent: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCustomManager {
    pub variables: Vec<(String, DumpCustomVariable)>,
    pub groups: Vec<(String, DumpCustomGroup)>,
    pub auto_buffer_local: Vec<String>,
}

// Abbrev
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrev {
    pub expansion: String,
    pub hook: Option<String>,
    pub count: usize,
    pub system: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrevTable {
    pub name: String,
    pub abbrevs: Vec<(String, DumpAbbrev)>,
    pub parent: Option<String>,
    pub case_fixed: bool,
    pub enable_quoting: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpAbbrevManager {
    pub tables: Vec<(String, DumpAbbrevTable)>,
    pub global_table_name: String,
    pub abbrev_mode: bool,
}

// Interactive
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpInteractiveSpec {
    pub code: String,
    pub prompt: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpInteractiveRegistry {
    pub specs: Vec<(String, DumpInteractiveSpec)>,
}

// Mode
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontLockKeyword {
    pub pattern: String,
    pub face: String,
    pub group: usize,
    pub override_: bool,
    pub laxmatch: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFontLockDefaults {
    pub keywords: Vec<DumpFontLockKeyword>,
    pub case_fold: bool,
    pub syntax_table: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMajorMode {
    pub name: String,
    pub pretty_name: String,
    pub parent: Option<String>,
    pub mode_hook: String,
    pub keymap_name: Option<String>,
    pub syntax_table_name: Option<String>,
    pub abbrev_table_name: Option<String>,
    pub font_lock: Option<DumpFontLockDefaults>,
    pub body: Option<DumpValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpMinorMode {
    pub name: String,
    pub lighter: Option<String>,
    pub keymap_name: Option<String>,
    pub global: bool,
    pub body: Option<DumpValue>,
}

// mode.rs has its own CustomVariable/CustomGroup — we mirror those separately
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpModeCustomVariable {
    pub name: String,
    pub default_value: DumpValue,
    pub doc: Option<String>,
    pub custom_type: DumpModeCustomType,
    pub group: Option<String>,
    pub set_function: Option<String>,
    pub get_function: Option<String>,
    pub tag: Option<String>,
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
    pub name: String,
    pub doc: Option<String>,
    pub parent: Option<String>,
    pub members: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpModeRegistry {
    pub major_modes: Vec<(String, DumpMajorMode)>,
    pub minor_modes: Vec<(String, DumpMinorMode)>,
    pub buffer_major_modes: Vec<(u64, String)>,
    pub buffer_minor_modes: Vec<(u64, Vec<String>)>,
    pub global_minor_modes: Vec<String>,
    pub auto_mode_alist: Vec<(String, String)>,
    pub custom_variables: Vec<(String, DumpModeCustomVariable)>,
    pub custom_groups: Vec<(String, DumpModeCustomGroup)>,
    pub fundamental_mode: String,
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
    pub name: String,
    pub coding_type: String,
    pub mnemonic: char,
    pub eol_type: DumpEolType,
    pub ascii_compatible_p: bool,
    pub charset_list: Vec<String>,
    pub post_read_conversion: Option<String>,
    pub pre_write_conversion: Option<String>,
    pub default_char: Option<char>,
    pub for_unibyte: bool,
    pub properties: Vec<(String, DumpValue)>,
    pub int_properties: Vec<(i64, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCodingSystemManager {
    pub systems: Vec<(String, DumpCodingSystemInfo)>,
    pub aliases: Vec<(String, String)>,
    pub priority: Vec<String>,
    pub keyboard_coding: String,
    pub terminal_coding: String,
}

// Charset
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetSubsetSpec {
    pub parent: String,
    pub parent_min_code: i64,
    pub parent_max_code: i64,
    pub offset: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpCharsetMethod {
    Offset(i64),
    Map(String),
    Subset(DumpCharsetSubsetSpec),
    Superset(Vec<(String, i64)>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetInfo {
    pub id: i64,
    pub name: String,
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
    pub unify_map: Option<String>,
    pub method: DumpCharsetMethod,
    pub plist: Vec<(String, DumpValue)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCharsetRegistry {
    pub charsets: Vec<DumpCharsetInfo>,
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpStoredFontSpec {
    pub family: Option<String>,
    pub registry: Option<String>,
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
    pub ordered_names: Vec<String>,
    pub alias_to_name: Vec<(String, String)>,
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
    pub name: String,
    pub foreground: Option<DumpColor>,
    pub background: Option<DumpColor>,
    pub family: Option<String>,
    pub height: Option<DumpFaceHeight>,
    pub weight: Option<u16>,
    pub slant: Option<DumpFontSlant>,
    pub underline: Option<DumpUnderline>,
    pub overline: Option<bool>,
    pub strike_through: Option<bool>,
    pub box_border: Option<DumpBoxBorder>,
    pub inverse_video: Option<bool>,
    pub stipple: Option<String>,
    pub extend: Option<bool>,
    pub inherit: Vec<String>,
    pub overstrike: bool,
    pub doc: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpFaceTable {
    pub faces: Vec<(String, DumpFace)>,
}

// Category
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCategoryTable {
    pub entries: Vec<(char, Vec<char>)>,
    pub descriptions: Vec<(char, String)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpCategoryManager {
    pub tables: Vec<(String, DumpCategoryTable)>,
    pub current_table: String,
}

// Rectangle
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpRectangleState {
    pub killed: Vec<String>,
}

// Kmacro
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpKmacroManager {
    pub current_macro: Vec<DumpValue>,
    pub last_macro: Option<Vec<DumpValue>>,
    pub macro_ring: Vec<Vec<DumpValue>>,
    pub counter: i64,
    pub counter_format: String,
}

// Register
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DumpRegisterContent {
    Text(String),
    Number(i64),
    Position { buffer: String, point: usize },
    Rectangle(Vec<String>),
    FrameConfig(DumpValue),
    File(String),
    KbdMacro(Vec<DumpValue>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpRegisterManager {
    pub registers: Vec<(char, DumpRegisterContent)>,
}

// Bookmark
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBookmark {
    pub name: String,
    pub filename: Option<String>,
    pub position: usize,
    pub front_context: Option<String>,
    pub rear_context: Option<String>,
    pub annotation: Option<String>,
    pub handler: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpBookmarkManager {
    pub bookmarks: Vec<(String, DumpBookmark)>,
    pub recent: Vec<String>,
}

// Variable watchers
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpVariableWatcherList {
    pub watchers: Vec<(String, Vec<DumpValue>)>,
}

// ---------------------------------------------------------------------------
// Top-level evaluator state
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DumpContextState {
    pub interner: DumpStringInterner,
    pub heap: DumpLispHeap,
    pub obarray: DumpObarray,
    pub dynamic: Vec<DumpOrderedSymMap>,
    pub lexenv: DumpValue,
    pub features: Vec<u32>,
    pub require_stack: Vec<u32>,
    pub buffers: DumpBufferManager,
    pub autoloads: DumpAutoloadManager,
    pub custom: DumpCustomManager,
    pub modes: DumpModeRegistry,
    pub coding_systems: DumpCodingSystemManager,
    pub charset_registry: DumpCharsetRegistry,
    pub fontset_registry: DumpFontsetRegistry,
    pub face_table: DumpFaceTable,
    pub category_manager: DumpCategoryManager,
    pub abbrevs: DumpAbbrevManager,
    pub interactive: DumpInteractiveRegistry,
    pub rectangle: DumpRectangleState,
    pub standard_syntax_table: DumpValue,
    pub current_local_map: DumpValue,
    pub kmacro: DumpKmacroManager,
    pub registers: DumpRegisterManager,
    pub bookmarks: DumpBookmarkManager,
    pub watchers: DumpVariableWatcherList,
    pub string_text_props: Vec<(u64, Vec<DumpPropertyInterval>)>,
}
