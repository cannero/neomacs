//! Conversions between runtime types and pdump snapshot types.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::DumpError;
use super::types::*;
use crate::buffer::buffer::{Buffer, BufferId, BufferManager, InsertionType, MarkerEntry};
use crate::buffer::buffer_text::BufferText;
use crate::buffer::overlay::{Overlay, OverlayList};
use crate::buffer::shared::SharedUndoState;
use crate::buffer::text_props::{PropertyInterval, TextPropertyTable};
// Undo state is now stored directly as a Lisp Value in buffer-local properties.
use crate::emacs_core::abbrev::{Abbrev, AbbrevManager, AbbrevTable};
use crate::emacs_core::advice::{VariableWatcher, VariableWatcherList};
use crate::emacs_core::autoload::{AutoloadEntry, AutoloadManager, AutoloadType};
use crate::emacs_core::bookmark::{Bookmark, BookmarkManager};
use crate::emacs_core::bytecode::chunk::ByteCodeFunction;
use crate::emacs_core::bytecode::opcode::Op;
use crate::emacs_core::charset::{
    CharsetInfoSnapshot, CharsetMethodSnapshot, CharsetRegistrySnapshot, restore_charset_registry,
    snapshot_charset_registry,
};
use crate::emacs_core::coding::{CodingSystemInfo, CodingSystemManager, EolType};
use crate::emacs_core::custom::CustomManager;
use crate::emacs_core::eval::Context;
use crate::emacs_core::expr::Expr;
use crate::emacs_core::fontset::{
    FontRepertory, FontSpecEntry, FontsetDataSnapshot, FontsetRangeEntrySnapshot,
    FontsetRegistrySnapshot, StoredFontSpec, restore_fontset_registry, snapshot_fontset_registry,
};
use crate::emacs_core::interactive::{InteractiveRegistry, InteractiveSpec};
use crate::emacs_core::intern::{self, SymId};
use crate::emacs_core::kmacro::KmacroManager;
use crate::emacs_core::mode::{
    self, CustomGroup as ModeCustomGroup, CustomType as ModeCustomType,
    CustomVariable as ModeCustomVariable, FontLockDefaults, FontLockKeyword, MajorMode, MinorMode,
    ModeRegistry,
};
use crate::emacs_core::rect::RectangleState;
use crate::emacs_core::register::{RegisterContent, RegisterManager};
use crate::emacs_core::symbol::{Obarray, SymbolData, SymbolValue};
use crate::emacs_core::syntax::{SyntaxClass, SyntaxEntry, SyntaxFlags, SyntaxTable};
use crate::emacs_core::value::{
    HashKey, HashTableTest, HashTableWeakness, LambdaData, LambdaParams, LispHashTable,
    OrderedRuntimeBindingMap, OrderedSymMap, RuntimeBindingValue, StringTextPropertyRun, Value,
};
use crate::emacs_core::value::{ValueKind, VecLikeType};
use crate::emacs_core::value::{
    get_string_text_properties_for_value, set_string_text_properties_for_value,
};
use crate::face::{
    BoxBorder, BoxStyle, Color, Face, FaceHeight, FaceTable, FontSlant, FontWeight, FontWidth,
    Underline, UnderlineStyle,
};
use crate::heap_types::LispString;
use crate::tagged::gc::with_tagged_heap;
use crate::tagged::header::{
    BufferObj, ByteCodeObj, CLOSURE_MIN_SLOTS, FloatObj, FrameObj, HashTableObj, LambdaObj,
    MacroObj, MarkerObj, OverlayObj, RecordObj, StringObj, SubrObj, TimerObj, VectorObj, WindowObj,
};
use crate::tagged::value::TaggedValue;

thread_local! {
    static PDUMP_DUMP_STATE: Cell<*mut TaggedDumpState> = const { Cell::new(std::ptr::null_mut()) };
    static PDUMP_LOAD_STATE: Cell<*mut TaggedLoadState> = const { Cell::new(std::ptr::null_mut()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TaggedObjId {
    index: u32,
}

struct TaggedDumpState {
    objects: Vec<Option<DumpHeapObject>>,
    object_ids: HashMap<usize, TaggedObjId>,
    float_ids: HashMap<usize, u32>,
    next_float_id: u32,
}

impl TaggedDumpState {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
            object_ids: HashMap::new(),
            float_ids: HashMap::new(),
            next_float_id: 1,
        }
    }

    fn finalize(self) -> DumpLispHeap {
        DumpLispHeap {
            objects: self
                .objects
                .into_iter()
                .map(|obj| obj.unwrap_or(DumpHeapObject::Free))
                .collect(),
        }
    }
}

struct TaggedLoadState {
    objects: Vec<DumpHeapObject>,
    values: Vec<Option<Value>>,
    populated: Vec<bool>,
    buffers: HashMap<u64, Value>,
    windows: HashMap<u64, Value>,
    frames: HashMap<u64, Value>,
    timers: HashMap<u64, Value>,
    floats: HashMap<u32, Value>,
}

impl TaggedLoadState {
    fn new(heap: &DumpLispHeap) -> Self {
        let len = heap.objects.len();
        Self {
            objects: heap.objects.clone(),
            values: vec![None; len],
            populated: vec![false; len],
            buffers: HashMap::new(),
            windows: HashMap::new(),
            frames: HashMap::new(),
            timers: HashMap::new(),
            floats: HashMap::new(),
        }
    }
}

fn dump_obj_id(id: TaggedObjId) -> DumpObjId {
    DumpObjId { index: id.index }
}

fn tagged_obj_id(id: &DumpObjId) -> TaggedObjId {
    TaggedObjId { index: id.index }
}

// ===========================================================================
// Dump direction: Runtime → Dump
// ===========================================================================

// --- Primitives ---

pub(crate) fn dump_sym_id(id: SymId) -> DumpSymId {
    DumpSymId(id.0)
}

fn with_dump_state<R>(f: impl FnOnce(&mut TaggedDumpState) -> R) -> R {
    PDUMP_DUMP_STATE.with(|state| {
        let ptr = state.get();
        assert!(!ptr.is_null(), "pdump dump state should be initialized");
        unsafe { f(&mut *ptr) }
    })
}

fn with_load_state<R>(f: impl FnOnce(&mut TaggedLoadState) -> R) -> R {
    PDUMP_LOAD_STATE.with(|state| {
        let ptr = state.get();
        assert!(!ptr.is_null(), "pdump load state should be initialized");
        unsafe { f(&mut *ptr) }
    })
}

fn value_to_obj_id(v: &Value) -> TaggedObjId {
    debug_assert!(v.is_heap_object());
    let bits = v.bits();
    with_dump_state(|state| {
        if let Some(id) = state.object_ids.get(&bits).copied() {
            return id;
        }

        let id = TaggedObjId {
            index: state.objects.len() as u32,
        };
        state.object_ids.insert(bits, id);
        state.objects.push(None);
        let dumped = dump_heap_object_from_value(*v);
        state.objects[id.index as usize] = Some(dumped);
        id
    })
}

fn obj_id_to_value(id: TaggedObjId) -> Value {
    with_load_state(|state| load_tagged_object(state, id))
}

fn dump_float_id(v: &Value) -> u32 {
    debug_assert!(v.is_float());
    let bits = v.bits();
    with_dump_state(|state| {
        if let Some(id) = state.float_ids.get(&bits).copied() {
            return id;
        }
        let id = state.next_float_id;
        state.next_float_id += 1;
        state.float_ids.insert(bits, id);
        id
    })
}

fn load_float_value(id: u32, value: f64) -> Value {
    with_load_state(|state| {
        *state
            .floats
            .entry(id)
            .or_insert_with(|| Value::make_float(value))
    })
}

pub(crate) fn dump_value(v: &Value) -> DumpValue {
    match v.kind() {
        ValueKind::Nil => DumpValue::Nil,
        ValueKind::T => DumpValue::True,
        ValueKind::Fixnum(n) => DumpValue::Int(n),
        ValueKind::Float => DumpValue::Float(v.xfloat(), dump_float_id(v)),
        ValueKind::Symbol(s) => DumpValue::Symbol(dump_sym_id(s)),
        ValueKind::String => DumpValue::Str(dump_obj_id(value_to_obj_id(v))),
        ValueKind::Cons => DumpValue::Cons(dump_obj_id(value_to_obj_id(v))),
        ValueKind::Veclike(VecLikeType::Vector) => {
            DumpValue::Vector(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Record) => {
            DumpValue::Record(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::HashTable) => {
            DumpValue::HashTable(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            DumpValue::Lambda(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Macro) => DumpValue::Macro(dump_obj_id(value_to_obj_id(v))),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let s = v.as_subr_id().unwrap();
            DumpValue::Subr(dump_sym_id(s))
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            DumpValue::ByteCode(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Marker) => {
            DumpValue::Marker(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Overlay) => {
            DumpValue::Overlay(dump_obj_id(value_to_obj_id(v)))
        }
        ValueKind::Veclike(VecLikeType::Buffer) => {
            DumpValue::Buffer(DumpBufferId(v.as_buffer_id().unwrap().0))
        }
        ValueKind::Veclike(VecLikeType::Window) => DumpValue::Window(v.as_window_id().unwrap()),
        ValueKind::Veclike(VecLikeType::Frame) => DumpValue::Frame(v.as_frame_id().unwrap()),
        ValueKind::Veclike(VecLikeType::Timer) => DumpValue::Timer(v.as_timer_id().unwrap()),
        ValueKind::Unknown => DumpValue::Nil,
    }
}

pub(crate) fn dump_opt_value(v: &Option<Value>) -> Option<DumpValue> {
    v.as_ref().map(dump_value)
}

// --- Expr ---

pub(crate) fn dump_expr(e: &Expr) -> DumpExpr {
    match e {
        Expr::Int(n) => DumpExpr::Int(*n),
        Expr::Float(f) => DumpExpr::Float(*f),
        Expr::Symbol(s) => DumpExpr::Symbol(dump_sym_id(*s)),
        Expr::ReaderLoadFileName => DumpExpr::ReaderLoadFileName,
        Expr::Keyword(s) => DumpExpr::Keyword(dump_sym_id(*s)),
        Expr::Str(s) => DumpExpr::Str(s.clone()),
        Expr::Char(c) => DumpExpr::Char(*c),
        Expr::List(items) => DumpExpr::List(items.iter().map(dump_expr).collect()),
        Expr::Vector(items) => DumpExpr::Vector(items.iter().map(dump_expr).collect()),
        Expr::DottedList(items, tail) => DumpExpr::DottedList(
            items.iter().map(dump_expr).collect(),
            Box::new(dump_expr(tail)),
        ),
        Expr::Bool(b) => DumpExpr::Bool(*b),
        Expr::OpaqueValueRef(idx) => {
            let val = crate::emacs_core::eval::OPAQUE_POOL.with(|pool| pool.borrow().get(*idx));
            DumpExpr::OpaqueValue(dump_value(&val))
        }
    }
}

// --- Op ---

pub(crate) fn dump_op(op: &Op) -> DumpOp {
    match *op {
        Op::Constant(n) => DumpOp::Constant(n),
        Op::Nil => DumpOp::Nil,
        Op::True => DumpOp::True,
        Op::Pop => DumpOp::Pop,
        Op::Dup => DumpOp::Dup,
        Op::StackRef(n) => DumpOp::StackRef(n),
        Op::StackSet(n) => DumpOp::StackSet(n),
        Op::DiscardN(n) => DumpOp::DiscardN(n),
        Op::VarRef(n) => DumpOp::VarRef(n),
        Op::VarSet(n) => DumpOp::VarSet(n),
        Op::VarBind(n) => DumpOp::VarBind(n),
        Op::Unbind(n) => DumpOp::Unbind(n),
        Op::Call(n) => DumpOp::Call(n),
        Op::Apply(n) => DumpOp::Apply(n),
        Op::Goto(n) => DumpOp::Goto(n),
        Op::GotoIfNil(n) => DumpOp::GotoIfNil(n),
        Op::GotoIfNotNil(n) => DumpOp::GotoIfNotNil(n),
        Op::GotoIfNilElsePop(n) => DumpOp::GotoIfNilElsePop(n),
        Op::GotoIfNotNilElsePop(n) => DumpOp::GotoIfNotNilElsePop(n),
        Op::Switch => DumpOp::Switch,
        Op::Return => DumpOp::Return,
        Op::Add => DumpOp::Add,
        Op::Sub => DumpOp::Sub,
        Op::Mul => DumpOp::Mul,
        Op::Div => DumpOp::Div,
        Op::Rem => DumpOp::Rem,
        Op::Add1 => DumpOp::Add1,
        Op::Sub1 => DumpOp::Sub1,
        Op::Negate => DumpOp::Negate,
        Op::Eqlsign => DumpOp::Eqlsign,
        Op::Gtr => DumpOp::Gtr,
        Op::Lss => DumpOp::Lss,
        Op::Leq => DumpOp::Leq,
        Op::Geq => DumpOp::Geq,
        Op::Max => DumpOp::Max,
        Op::Min => DumpOp::Min,
        Op::Car => DumpOp::Car,
        Op::Cdr => DumpOp::Cdr,
        Op::Cons => DumpOp::Cons,
        Op::List(n) => DumpOp::List(n),
        Op::Length => DumpOp::Length,
        Op::Nth => DumpOp::Nth,
        Op::Nthcdr => DumpOp::Nthcdr,
        Op::Setcar => DumpOp::Setcar,
        Op::Setcdr => DumpOp::Setcdr,
        Op::CarSafe => DumpOp::CarSafe,
        Op::CdrSafe => DumpOp::CdrSafe,
        Op::Elt => DumpOp::Elt,
        Op::Nconc => DumpOp::Nconc,
        Op::Nreverse => DumpOp::Nreverse,
        Op::Member => DumpOp::Member,
        Op::Memq => DumpOp::Memq,
        Op::Assq => DumpOp::Assq,
        Op::Symbolp => DumpOp::Symbolp,
        Op::Consp => DumpOp::Consp,
        Op::Stringp => DumpOp::Stringp,
        Op::Listp => DumpOp::Listp,
        Op::Integerp => DumpOp::Integerp,
        Op::Numberp => DumpOp::Numberp,
        Op::Null => DumpOp::Null,
        Op::Not => DumpOp::Not,
        Op::Eq => DumpOp::Eq,
        Op::Equal => DumpOp::Equal,
        Op::Concat(n) => DumpOp::Concat(n),
        Op::Substring => DumpOp::Substring,
        Op::StringEqual => DumpOp::StringEqual,
        Op::StringLessp => DumpOp::StringLessp,
        Op::Aref => DumpOp::Aref,
        Op::Aset => DumpOp::Aset,
        Op::SymbolValue => DumpOp::SymbolValue,
        Op::SymbolFunction => DumpOp::SymbolFunction,
        Op::Set => DumpOp::Set,
        Op::Fset => DumpOp::Fset,
        Op::Get => DumpOp::Get,
        Op::Put => DumpOp::Put,
        Op::PushConditionCase(n) => DumpOp::PushConditionCase(n),
        Op::PushConditionCaseRaw(n) => DumpOp::PushConditionCaseRaw(n),
        Op::PushCatch(n) => DumpOp::PushCatch(n),
        Op::PopHandler => DumpOp::PopHandler,
        Op::UnwindProtectPop => DumpOp::UnwindProtectPop,
        Op::Throw => DumpOp::Throw,
        Op::SaveCurrentBuffer => DumpOp::SaveCurrentBuffer,
        Op::SaveExcursion => DumpOp::SaveExcursion,
        Op::SaveRestriction => DumpOp::SaveRestriction,
        Op::MakeClosure(n) => DumpOp::MakeClosure(n),
        Op::CallBuiltin(a, b) => DumpOp::CallBuiltin(a, b),
    }
}

// --- Lambda / ByteCode ---

pub(crate) fn dump_lambda_params(p: &LambdaParams) -> DumpLambdaParams {
    DumpLambdaParams {
        required: p.required.iter().map(|s| dump_sym_id(*s)).collect(),
        optional: p.optional.iter().map(|s| dump_sym_id(*s)).collect(),
        rest: p.rest.map(|s| dump_sym_id(s)),
    }
}

pub(crate) fn dump_bytecode(bc: &ByteCodeFunction) -> DumpByteCodeFunction {
    DumpByteCodeFunction {
        ops: bc.ops.iter().map(dump_op).collect(),
        constants: bc.constants.iter().map(dump_value).collect(),
        max_stack: bc.max_stack,
        params: dump_lambda_params(&bc.params),
        lexical: bc.lexical,
        env: dump_opt_value(&bc.env),
        gnu_byte_offset_map: bc.gnu_byte_offset_map.as_ref().map(|map| {
            map.iter()
                .map(|(byte_off, instr_idx)| (*byte_off as u32, *instr_idx as u32))
                .collect()
        }),
        docstring: bc.docstring.clone(),
        doc_form: dump_opt_value(&bc.doc_form),
        interactive: dump_opt_value(&bc.interactive),
    }
}

// --- Hash tables ---

pub(crate) fn dump_hash_key(k: &HashKey) -> DumpHashKey {
    match k {
        HashKey::Nil => DumpHashKey::Nil,
        HashKey::True => DumpHashKey::True,
        HashKey::Int(n) => DumpHashKey::Int(*n),
        HashKey::Float(bits) => DumpHashKey::Float(*bits),
        HashKey::FloatEq(bits, id) => DumpHashKey::FloatEq(*bits, *id),
        HashKey::Symbol(s) => DumpHashKey::Symbol(dump_sym_id(*s)),
        HashKey::Keyword(s) => DumpHashKey::Keyword(dump_sym_id(*s)),
        HashKey::Char(c) => DumpHashKey::Char(*c),
        HashKey::Window(w) => DumpHashKey::Window(*w),
        HashKey::Frame(f) => DumpHashKey::Frame(*f),
        HashKey::Ptr(p) => {
            let value = TaggedValue(*p);
            if value.is_heap_object() {
                let id = value_to_obj_id(&value);
                DumpHashKey::ObjId(id.index)
            } else {
                DumpHashKey::Ptr(*p as u64)
            }
        }
        HashKey::EqualCons(a, b) => {
            DumpHashKey::EqualCons(Box::new(dump_hash_key(a)), Box::new(dump_hash_key(b)))
        }
        HashKey::EqualVec(v) => DumpHashKey::EqualVec(v.iter().map(dump_hash_key).collect()),
        HashKey::Cycle(index) => DumpHashKey::Cycle(*index),
        HashKey::Text(text) => DumpHashKey::Text(text.clone()),
    }
}

pub(crate) fn dump_hash_table_test(t: &HashTableTest) -> DumpHashTableTest {
    match t {
        HashTableTest::Eq => DumpHashTableTest::Eq,
        HashTableTest::Eql => DumpHashTableTest::Eql,
        HashTableTest::Equal => DumpHashTableTest::Equal,
    }
}

pub(crate) fn dump_hash_table_weakness(w: &HashTableWeakness) -> DumpHashTableWeakness {
    match w {
        HashTableWeakness::Key => DumpHashTableWeakness::Key,
        HashTableWeakness::Value => DumpHashTableWeakness::Value,
        HashTableWeakness::KeyOrValue => DumpHashTableWeakness::KeyOrValue,
        HashTableWeakness::KeyAndValue => DumpHashTableWeakness::KeyAndValue,
    }
}

pub(crate) fn dump_hash_table(ht: &LispHashTable) -> DumpLispHashTable {
    DumpLispHashTable {
        test: dump_hash_table_test(&ht.test),
        test_name: ht.test_name.map(|s| dump_sym_id(s)),
        size: ht.size,
        weakness: ht.weakness.as_ref().map(dump_hash_table_weakness),
        rehash_size: ht.rehash_size,
        rehash_threshold: ht.rehash_threshold,
        entries: ht
            .data
            .iter()
            .map(|(k, v)| (dump_hash_key(k), dump_value(v)))
            .collect(),
        key_snapshots: ht
            .key_snapshots
            .iter()
            .map(|(k, v)| (dump_hash_key(k), dump_value(v)))
            .collect(),
        insertion_order: ht.insertion_order.iter().map(dump_hash_key).collect(),
    }
}

// --- Heap objects ---

fn dump_closure_slots(value: Value) -> Vec<DumpValue> {
    value
        .closure_slots()
        .map(|slots| slots.iter().map(dump_value).collect())
        .unwrap_or_default()
}

fn dump_heap_object_from_value(value: Value) -> DumpHeapObject {
    match value.kind() {
        ValueKind::Cons => DumpHeapObject::Cons {
            car: dump_value(&value.cons_car()),
            cdr: dump_value(&value.cons_cdr()),
        },
        ValueKind::String => {
            let string = value.as_lisp_string().expect("string");
            DumpHeapObject::Str {
                text: string.as_str().to_owned(),
                multibyte: string.multibyte,
                text_props: get_string_text_properties_for_value(value)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|run| DumpStringTextPropertyRun {
                        start: run.start,
                        end: run.end,
                        plist: dump_value(&run.plist),
                    })
                    .collect(),
            }
        }
        ValueKind::Float => DumpHeapObject::Float(value.xfloat()),
        ValueKind::Veclike(VecLikeType::Vector) => DumpHeapObject::Vector(
            value
                .as_vector_data()
                .expect("vector")
                .iter()
                .map(dump_value)
                .collect(),
        ),
        ValueKind::Veclike(VecLikeType::HashTable) => {
            DumpHeapObject::HashTable(dump_hash_table(value.as_hash_table().expect("hash-table")))
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            DumpHeapObject::Lambda(dump_closure_slots(value))
        }
        ValueKind::Veclike(VecLikeType::Macro) => DumpHeapObject::Macro(dump_closure_slots(value)),
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            DumpHeapObject::ByteCode(dump_bytecode(value.get_bytecode_data().expect("bytecode")))
        }
        ValueKind::Veclike(VecLikeType::Record) => DumpHeapObject::Record(
            value
                .as_record_data()
                .expect("record")
                .iter()
                .map(dump_value)
                .collect(),
        ),
        ValueKind::Veclike(VecLikeType::Overlay) => {
            DumpHeapObject::Overlay(dump_overlay(value.as_overlay_data().expect("overlay")))
        }
        ValueKind::Veclike(VecLikeType::Marker) => {
            DumpHeapObject::Marker(dump_marker_object(value.as_marker_data().expect("marker")))
        }
        ValueKind::Veclike(VecLikeType::Buffer) => {
            DumpHeapObject::Buffer(DumpBufferId(value.as_buffer_id().expect("buffer").0))
        }
        ValueKind::Veclike(VecLikeType::Window) => {
            DumpHeapObject::Window(value.as_window_id().expect("window"))
        }
        ValueKind::Veclike(VecLikeType::Frame) => {
            DumpHeapObject::Frame(value.as_frame_id().expect("frame"))
        }
        ValueKind::Veclike(VecLikeType::Timer) => {
            DumpHeapObject::Timer(value.as_timer_id().expect("timer"))
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            let ptr = value.as_veclike_ptr().expect("subr") as *const SubrObj;
            let subr = unsafe { &*ptr };
            DumpHeapObject::Subr {
                name: dump_sym_id(subr.name),
                min_args: subr.min_args,
                max_args: subr.max_args,
            }
        }
        _ => DumpHeapObject::Free,
    }
}

// --- Interner ---

pub(crate) fn dump_interner() -> DumpStringInterner {
    let interner = intern::dump_runtime_interner();
    DumpStringInterner {
        strings: interner.strings().iter().map(|s| (*s).to_owned()).collect(),
    }
}

// --- Symbol / Obarray ---

fn dump_symbol_value(sv: &SymbolValue) -> DumpSymbolValue {
    match sv {
        SymbolValue::Plain(v) => DumpSymbolValue::Plain(dump_opt_value(v)),
        SymbolValue::Alias(target) => DumpSymbolValue::Alias(dump_sym_id(*target)),
        SymbolValue::BufferLocal {
            default,
            local_if_set,
        } => DumpSymbolValue::BufferLocal {
            default: dump_opt_value(default),
            local_if_set: *local_if_set,
        },
        SymbolValue::Forwarded => DumpSymbolValue::Forwarded,
    }
}

pub(crate) fn dump_symbol_data(sd: &SymbolData) -> DumpSymbolData {
    DumpSymbolData {
        name: dump_sym_id(sd.name),
        value: None,
        symbol_value: Some(dump_symbol_value(&sd.value)),
        function: dump_opt_value(&sd.function),
        plist: sd
            .plist
            .iter()
            .map(|(k, v)| (dump_sym_id(*k), dump_value(v)))
            .collect(),
        special: sd.special,
        constant: sd.constant,
    }
}

pub(crate) fn dump_obarray(ob: &Obarray) -> DumpObarray {
    DumpObarray {
        symbols: ob
            .iter_symbols()
            .map(|(id, sd)| (id.0, dump_symbol_data(sd)))
            .collect(),
        global_members: ob.global_members().iter().map(|id| id.0).collect(),
        function_unbound: ob.function_unbound_set().iter().map(|id| id.0).collect(),
        function_epoch: ob.function_epoch(),
    }
}

// --- OrderedSymMap ---

fn dump_runtime_binding_value(value: &RuntimeBindingValue) -> DumpRuntimeBindingValue {
    match value {
        RuntimeBindingValue::Bound(value) => DumpRuntimeBindingValue::Bound(dump_value(value)),
        RuntimeBindingValue::Void => DumpRuntimeBindingValue::Void,
    }
}

fn load_runtime_binding_value(value: &DumpRuntimeBindingValue) -> RuntimeBindingValue {
    match value {
        DumpRuntimeBindingValue::Bound(value) => RuntimeBindingValue::Bound(load_value(value)),
        DumpRuntimeBindingValue::Void => RuntimeBindingValue::Void,
    }
}

pub(crate) fn dump_ordered_sym_map(m: &OrderedRuntimeBindingMap) -> DumpOrderedSymMap {
    DumpOrderedSymMap {
        entries: m
            .iter()
            .map(|(k, v)| (dump_sym_id(*k), dump_runtime_binding_value(v)))
            .collect(),
    }
}

// --- Buffer types ---

fn dump_insertion_type(it: &InsertionType) -> DumpInsertionType {
    match it {
        InsertionType::Before => DumpInsertionType::Before,
        InsertionType::After => DumpInsertionType::After,
    }
}

fn dump_marker(m: &MarkerEntry) -> DumpMarkerEntry {
    DumpMarkerEntry {
        id: m.id,
        buffer_id: m.buffer_id.0,
        byte_pos: m.byte_pos,
        char_pos: Some(m.char_pos),
        insertion_type: dump_insertion_type(&m.insertion_type),
    }
}

fn dump_property_interval(pi: &PropertyInterval) -> DumpPropertyInterval {
    DumpPropertyInterval {
        start: pi.start,
        end: pi.end,
        properties: pi
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), dump_value(v)))
            .collect(),
    }
}

fn dump_text_property_table(tpt: &TextPropertyTable) -> DumpTextPropertyTable {
    DumpTextPropertyTable {
        intervals: tpt
            .dump_intervals()
            .into_iter()
            .map(|iv| dump_property_interval(&iv))
            .collect(),
    }
}

fn dump_overlay(o: &Overlay) -> DumpOverlay {
    DumpOverlay {
        plist: dump_value(&o.plist),
        buffer: o.buffer.map(|id| DumpBufferId(id.0)),
        start: o.start,
        end: o.end,
        front_advance: o.front_advance,
        rear_advance: o.rear_advance,
    }
}

fn dump_marker_object(marker: &crate::heap_types::MarkerData) -> DumpMarker {
    DumpMarker {
        buffer: marker.buffer.map(|id| DumpBufferId(id.0)),
        position: marker.position,
        insertion_type: marker.insertion_type,
        marker_id: marker.marker_id,
    }
}

fn dump_overlay_list(ol: &OverlayList) -> DumpOverlayList {
    DumpOverlayList {
        overlays: ol
            .dump_overlays()
            .iter()
            .filter_map(|v| v.as_overlay_data())
            .map(|data| dump_overlay(data))
            .collect(),
    }
}

fn dump_syntax_class(c: &SyntaxClass) -> DumpSyntaxClass {
    match c {
        SyntaxClass::Whitespace => DumpSyntaxClass::Whitespace,
        SyntaxClass::Word => DumpSyntaxClass::Word,
        SyntaxClass::Symbol => DumpSyntaxClass::Symbol,
        SyntaxClass::Punctuation => DumpSyntaxClass::Punctuation,
        SyntaxClass::Open => DumpSyntaxClass::Open,
        SyntaxClass::Close => DumpSyntaxClass::Close,
        SyntaxClass::Quote => DumpSyntaxClass::Prefix,
        SyntaxClass::StringDelim => DumpSyntaxClass::StringDelim,
        SyntaxClass::Math => DumpSyntaxClass::MathDelim,
        SyntaxClass::Escape => DumpSyntaxClass::Escape,
        SyntaxClass::CharQuote => DumpSyntaxClass::CharQuote,
        SyntaxClass::Comment => DumpSyntaxClass::Comment,
        SyntaxClass::EndComment => DumpSyntaxClass::EndComment,
        SyntaxClass::InheritStd => DumpSyntaxClass::InheritStandard,
        SyntaxClass::CommentFence => DumpSyntaxClass::Generic,
        SyntaxClass::StringFence => DumpSyntaxClass::StringFence,
    }
}

fn dump_syntax_entry(se: &SyntaxEntry) -> DumpSyntaxEntry {
    DumpSyntaxEntry {
        class: dump_syntax_class(&se.class),
        matching_char: se.matching_char,
        flags: se.flags.bits(),
    }
}

fn dump_syntax_table(st: &SyntaxTable) -> DumpSyntaxTable {
    DumpSyntaxTable {
        entries: st
            .dump_entries()
            .iter()
            .map(|(c, e)| (*c, dump_syntax_entry(e)))
            .collect(),
        parent: st
            .dump_parent()
            .as_ref()
            .map(|p| Box::new(dump_syntax_table(p))),
    }
}

// dump_undo_record and dump_undo_list removed — undo state is now a
// buffer-local Lisp Value serialized through the properties map.

fn dump_buffer(buf: &Buffer) -> DumpBuffer {
    let is_shared_text_owner = buf.base_buffer.is_none();
    DumpBuffer {
        id: DumpBufferId(buf.id.0),
        name: buf.name.clone(),
        base_buffer: buf.base_buffer.map(|id| DumpBufferId(id.0)),
        text: DumpGapBuffer {
            text: buf.text.dump_text(),
        },
        pt: buf.pt,
        pt_char: Some(buf.pt_char),
        mark: buf.mark,
        mark_char: buf.mark_char,
        begv: buf.begv,
        begv_char: Some(buf.begv_char),
        zv: buf.zv,
        zv_char: Some(buf.zv_char),
        modified: buf.modified,
        modified_tick: buf.modified_tick,
        chars_modified_tick: buf.chars_modified_tick,
        save_modified_tick: Some(buf.save_modified_tick),
        autosave_modified_tick: Some(buf.autosave_modified_tick),
        last_window_start: Some(buf.last_window_start),
        read_only: buf.read_only,
        multibyte: buf.multibyte,
        file_name: buf.file_name.clone(),
        auto_save_file_name: buf.auto_save_file_name.clone(),
        markers: if is_shared_text_owner {
            buf.text
                .marker_entries_snapshot()
                .iter()
                .map(dump_marker)
                .collect()
        } else {
            Vec::new()
        },
        state_pt_marker: buf.state_markers.map(|markers| markers.pt_marker),
        state_begv_marker: buf.state_markers.map(|markers| markers.begv_marker),
        state_zv_marker: buf.state_markers.map(|markers| markers.zv_marker),
        properties: buf
            .ordered_buffer_local_bindings()
            .into_iter()
            .map(|(k, v)| (k, dump_runtime_binding_value(&v)))
            .collect(),
        local_binding_names: buf.ordered_buffer_local_names(),
        local_map: dump_value(&buf.local_map()),
        text_props: if is_shared_text_owner {
            dump_text_property_table(&buf.text.text_props_snapshot())
        } else {
            dump_text_property_table(&TextPropertyTable::new())
        },
        overlays: dump_overlay_list(&buf.overlays),
        syntax_table: dump_syntax_table(&buf.syntax_table),
        undo_list: None,
    }
}

pub(crate) fn dump_buffer_manager(bm: &BufferManager) -> DumpBufferManager {
    DumpBufferManager {
        buffers: bm
            .dump_buffers()
            .iter()
            .map(|(id, buf)| (DumpBufferId(id.0), dump_buffer(buf)))
            .collect(),
        current: bm.dump_current().map(|id| DumpBufferId(id.0)),
        next_id: bm.dump_next_id(),
        next_marker_id: bm.dump_next_marker_id(),
    }
}

// --- Sub-managers ---

pub(crate) fn dump_autoload_manager(am: &AutoloadManager) -> DumpAutoloadManager {
    DumpAutoloadManager {
        entries: am
            .dump_entries()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    DumpAutoloadEntry {
                        name: v.name.clone(),
                        file: v.file.clone(),
                        docstring: v.docstring.clone(),
                        interactive: v.interactive,
                        autoload_type: match v.autoload_type {
                            AutoloadType::Function => DumpAutoloadType::Function,
                            AutoloadType::Macro => DumpAutoloadType::Macro,
                            AutoloadType::Keymap => DumpAutoloadType::Keymap,
                        },
                    },
                )
            })
            .collect(),
        after_load: am
            .dump_after_load()
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(dump_value).collect()))
            .collect(),
        loaded_files: am.dump_loaded_files().to_vec(),
        obsolete_functions: am
            .dump_obsolete_functions()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        obsolete_variables: am
            .dump_obsolete_variables()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

pub(crate) fn dump_custom_manager(cm: &CustomManager) -> DumpCustomManager {
    DumpCustomManager {
        auto_buffer_local: cm.auto_buffer_local.iter().cloned().collect(),
    }
}

fn dump_font_lock_keyword(kw: &FontLockKeyword) -> DumpFontLockKeyword {
    DumpFontLockKeyword {
        pattern: kw.pattern.clone(),
        face: kw.face.clone(),
        group: kw.group,
        override_: kw.override_,
        laxmatch: kw.laxmatch,
    }
}

fn dump_font_lock_defaults(fld: &FontLockDefaults) -> DumpFontLockDefaults {
    DumpFontLockDefaults {
        keywords: fld.keywords.iter().map(dump_font_lock_keyword).collect(),
        case_fold: fld.case_fold,
        syntax_table: fld.syntax_table.clone(),
    }
}

fn dump_mode_custom_type(ct: &ModeCustomType) -> DumpModeCustomType {
    match ct {
        ModeCustomType::Boolean => DumpModeCustomType::Boolean,
        ModeCustomType::Integer => DumpModeCustomType::Integer,
        ModeCustomType::Float => DumpModeCustomType::Float,
        ModeCustomType::String => DumpModeCustomType::String,
        ModeCustomType::Symbol => DumpModeCustomType::Symbol,
        ModeCustomType::Sexp => DumpModeCustomType::Sexp,
        ModeCustomType::Choice(choices) => DumpModeCustomType::Choice(
            choices
                .iter()
                .map(|(s, v)| (s.clone(), dump_value(v)))
                .collect(),
        ),
        ModeCustomType::List(inner) => {
            DumpModeCustomType::List(Box::new(dump_mode_custom_type(inner)))
        }
        ModeCustomType::Alist(k, v) => DumpModeCustomType::Alist(
            Box::new(dump_mode_custom_type(k)),
            Box::new(dump_mode_custom_type(v)),
        ),
        ModeCustomType::Plist(k, v) => DumpModeCustomType::Plist(
            Box::new(dump_mode_custom_type(k)),
            Box::new(dump_mode_custom_type(v)),
        ),
        ModeCustomType::Color => DumpModeCustomType::Color,
        ModeCustomType::Face => DumpModeCustomType::Face,
        ModeCustomType::File => DumpModeCustomType::File,
        ModeCustomType::Directory => DumpModeCustomType::Directory,
        ModeCustomType::Function => DumpModeCustomType::Function,
        ModeCustomType::Variable => DumpModeCustomType::Variable,
        ModeCustomType::Hook => DumpModeCustomType::Hook,
        ModeCustomType::Coding => DumpModeCustomType::Coding,
    }
}

pub(crate) fn dump_mode_registry(mr: &ModeRegistry) -> DumpModeRegistry {
    DumpModeRegistry {
        major_modes: mr
            .dump_major_modes()
            .iter()
            .map(|(k, m)| {
                (
                    k.clone(),
                    DumpMajorMode {
                        name: m.name.clone(),
                        pretty_name: m.pretty_name.clone(),
                        parent: m.parent.clone(),
                        mode_hook: m.mode_hook.clone(),
                        keymap_name: m.keymap_name.clone(),
                        syntax_table_name: m.syntax_table_name.clone(),
                        abbrev_table_name: m.abbrev_table_name.clone(),
                        font_lock: m.font_lock.as_ref().map(dump_font_lock_defaults),
                        body: dump_opt_value(&m.body),
                    },
                )
            })
            .collect(),
        minor_modes: mr
            .dump_minor_modes()
            .iter()
            .map(|(k, m)| {
                (
                    k.clone(),
                    DumpMinorMode {
                        name: m.name.clone(),
                        lighter: m.lighter.clone(),
                        keymap_name: m.keymap_name.clone(),
                        global: m.global,
                        body: dump_opt_value(&m.body),
                    },
                )
            })
            .collect(),
        buffer_major_modes: mr
            .dump_buffer_major_modes()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        buffer_minor_modes: mr
            .dump_buffer_minor_modes()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        global_minor_modes: mr.dump_global_minor_modes().to_vec(),
        auto_mode_alist: mr.dump_auto_mode_alist().to_vec(),
        custom_variables: mr
            .dump_custom_variables()
            .iter()
            .map(|(k, cv)| {
                (
                    k.clone(),
                    DumpModeCustomVariable {
                        name: cv.name.clone(),
                        default_value: dump_value(&cv.default_value),
                        doc: cv.doc.clone(),
                        custom_type: dump_mode_custom_type(&cv.type_),
                        group: cv.group.clone(),
                        set_function: cv.set_function.clone(),
                        get_function: cv.get_function.clone(),
                        tag: cv.tag.clone(),
                    },
                )
            })
            .collect(),
        custom_groups: mr
            .dump_custom_groups()
            .iter()
            .map(|(k, g)| {
                (
                    k.clone(),
                    DumpModeCustomGroup {
                        name: g.name.clone(),
                        doc: g.doc.clone(),
                        parent: g.parent.clone(),
                        members: g.members.clone(),
                    },
                )
            })
            .collect(),
        fundamental_mode: mr.dump_fundamental_mode().to_owned(),
    }
}

fn dump_eol_type(e: &EolType) -> DumpEolType {
    match e {
        EolType::Unix => DumpEolType::Unix,
        EolType::Dos => DumpEolType::Dos,
        EolType::Mac => DumpEolType::Mac,
        EolType::Undecided => DumpEolType::Undecided,
    }
}

pub(crate) fn dump_coding_system_manager(csm: &CodingSystemManager) -> DumpCodingSystemManager {
    DumpCodingSystemManager {
        systems: csm
            .systems
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    DumpCodingSystemInfo {
                        name: v.name.clone(),
                        coding_type: v.coding_type.clone(),
                        mnemonic: v.mnemonic,
                        eol_type: dump_eol_type(&v.eol_type),
                        ascii_compatible_p: v.ascii_compatible_p,
                        charset_list: v.charset_list.clone(),
                        post_read_conversion: v.post_read_conversion.clone(),
                        pre_write_conversion: v.pre_write_conversion.clone(),
                        default_char: v.default_char,
                        for_unibyte: v.for_unibyte,
                        properties: v
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), dump_value(v)))
                            .collect(),
                        int_properties: v
                            .int_properties
                            .iter()
                            .map(|(k, v)| (*k, dump_value(v)))
                            .collect(),
                    },
                )
            })
            .collect(),
        aliases: csm
            .aliases
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        priority: csm.priority.clone(),
        keyboard_coding: csm.dump_keyboard_coding().to_owned(),
        terminal_coding: csm.dump_terminal_coding().to_owned(),
    }
}

pub(crate) fn dump_charset_registry() -> DumpCharsetRegistry {
    let snapshot = snapshot_charset_registry();
    DumpCharsetRegistry {
        charsets: snapshot
            .charsets
            .into_iter()
            .map(|info| DumpCharsetInfo {
                id: info.id,
                name: info.name,
                dimension: info.dimension,
                code_space: info.code_space,
                min_code: info.min_code,
                max_code: info.max_code,
                iso_final_char: info.iso_final_char,
                iso_revision: info.iso_revision,
                emacs_mule_id: info.emacs_mule_id,
                ascii_compatible_p: info.ascii_compatible_p,
                supplementary_p: info.supplementary_p,
                invalid_code: info.invalid_code,
                unify_map: info.unify_map,
                method: match info.method {
                    CharsetMethodSnapshot::Offset(offset) => DumpCharsetMethod::Offset(offset),
                    CharsetMethodSnapshot::Map(map_name) => DumpCharsetMethod::Map(map_name),
                    CharsetMethodSnapshot::Subset(subset) => {
                        DumpCharsetMethod::Subset(DumpCharsetSubsetSpec {
                            parent: subset.parent,
                            parent_min_code: subset.parent_min_code,
                            parent_max_code: subset.parent_max_code,
                            offset: subset.offset,
                        })
                    }
                    CharsetMethodSnapshot::Superset(members) => {
                        DumpCharsetMethod::Superset(members)
                    }
                },
                plist: info
                    .plist
                    .into_iter()
                    .map(|(key, value)| (key, dump_value(&value)))
                    .collect(),
            })
            .collect(),
        priority: snapshot.priority,
        next_id: snapshot.next_id,
    }
}

fn dump_font_width(width: &FontWidth) -> DumpFontWidth {
    match width {
        FontWidth::UltraCondensed => DumpFontWidth::UltraCondensed,
        FontWidth::ExtraCondensed => DumpFontWidth::ExtraCondensed,
        FontWidth::Condensed => DumpFontWidth::Condensed,
        FontWidth::SemiCondensed => DumpFontWidth::SemiCondensed,
        FontWidth::Normal => DumpFontWidth::Normal,
        FontWidth::SemiExpanded => DumpFontWidth::SemiExpanded,
        FontWidth::Expanded => DumpFontWidth::Expanded,
        FontWidth::ExtraExpanded => DumpFontWidth::ExtraExpanded,
        FontWidth::UltraExpanded => DumpFontWidth::UltraExpanded,
    }
}

fn dump_font_repertory(repertory: FontRepertory) -> DumpFontRepertory {
    match repertory {
        FontRepertory::Charset(name) => DumpFontRepertory::Charset(name),
        FontRepertory::CharTableRanges(ranges) => DumpFontRepertory::CharTableRanges(ranges),
    }
}

fn dump_stored_font_spec(spec: StoredFontSpec) -> DumpStoredFontSpec {
    DumpStoredFontSpec {
        family: spec.family,
        registry: spec.registry,
        lang: spec.lang,
        weight: spec.weight.map(|weight| weight.0),
        slant: spec.slant.map(|slant| dump_font_slant(&slant)),
        width: spec.width.map(|width| dump_font_width(&width)),
        repertory: spec.repertory.map(dump_font_repertory),
    }
}

fn dump_font_spec_entry(entry: FontSpecEntry) -> DumpFontSpecEntry {
    match entry {
        FontSpecEntry::Font(spec) => DumpFontSpecEntry::Font(dump_stored_font_spec(spec)),
        FontSpecEntry::ExplicitNone => DumpFontSpecEntry::ExplicitNone,
    }
}

pub(crate) fn dump_fontset_registry() -> DumpFontsetRegistry {
    let snapshot = snapshot_fontset_registry();
    DumpFontsetRegistry {
        ordered_names: snapshot.ordered_names,
        alias_to_name: snapshot.alias_to_name,
        fontsets: snapshot
            .fontsets
            .into_iter()
            .map(|(name, data)| {
                (
                    name,
                    DumpFontsetData {
                        ranges: data
                            .ranges
                            .into_iter()
                            .map(|range| DumpFontsetRangeEntry {
                                from: range.from,
                                to: range.to,
                                entries: range
                                    .entries
                                    .into_iter()
                                    .map(dump_font_spec_entry)
                                    .collect(),
                            })
                            .collect(),
                        fallback: data
                            .fallback
                            .map(|entries| entries.into_iter().map(dump_font_spec_entry).collect()),
                    },
                )
            })
            .collect(),
        generation: snapshot.generation,
    }
}

fn dump_color(c: &Color) -> DumpColor {
    DumpColor {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn dump_font_slant(s: &FontSlant) -> DumpFontSlant {
    match s {
        FontSlant::Normal => DumpFontSlant::Normal,
        FontSlant::Italic => DumpFontSlant::Italic,
        FontSlant::Oblique => DumpFontSlant::Oblique,
        FontSlant::ReverseItalic => DumpFontSlant::ReverseItalic,
        FontSlant::ReverseOblique => DumpFontSlant::ReverseOblique,
    }
}

fn dump_underline_style(s: &UnderlineStyle) -> DumpUnderlineStyle {
    match s {
        UnderlineStyle::Line => DumpUnderlineStyle::Line,
        UnderlineStyle::Wave => DumpUnderlineStyle::Wave,
        UnderlineStyle::Dot => DumpUnderlineStyle::Dot,
        UnderlineStyle::Dash => DumpUnderlineStyle::Dash,
        UnderlineStyle::DoubleLine => DumpUnderlineStyle::DoubleLine,
    }
}

fn dump_box_style(s: &BoxStyle) -> DumpBoxStyle {
    match s {
        BoxStyle::Flat => DumpBoxStyle::Flat,
        BoxStyle::Raised => DumpBoxStyle::Raised,
        BoxStyle::Pressed => DumpBoxStyle::Pressed,
    }
}

fn dump_face_height(h: &FaceHeight) -> DumpFaceHeight {
    match h {
        FaceHeight::Absolute(n) => DumpFaceHeight::Absolute(*n),
        FaceHeight::Relative(f) => DumpFaceHeight::Relative(*f),
    }
}

fn dump_face(f: &Face) -> DumpFace {
    DumpFace {
        name: f.name.clone(),
        foreground: f.foreground.map(|c| dump_color(&c)),
        background: f.background.map(|c| dump_color(&c)),
        family: f.family.clone(),
        height: f.height.as_ref().map(dump_face_height),
        weight: f.weight.map(|w| w.0),
        slant: f.slant.as_ref().map(dump_font_slant),
        underline: f.underline.as_ref().map(|u| DumpUnderline {
            style: dump_underline_style(&u.style),
            color: u.color.map(|c| dump_color(&c)),
            position: u.position,
        }),
        overline: f.overline,
        strike_through: f.strike_through,
        box_border: f.box_border.as_ref().map(|b| DumpBoxBorder {
            color: b.color.map(|c| dump_color(&c)),
            width: b.width,
            style: dump_box_style(&b.style),
        }),
        inverse_video: f.inverse_video,
        stipple: f.stipple.clone(),
        extend: f.extend,
        inherit: f.inherit.clone(),
        overstrike: f.overstrike,
        doc: f.doc.clone(),
    }
}

pub(crate) fn dump_face_table(ft: &FaceTable) -> DumpFaceTable {
    DumpFaceTable {
        faces: ft
            .dump_faces()
            .iter()
            .map(|(k, f)| (k.clone(), dump_face(f)))
            .collect(),
    }
}

pub(crate) fn dump_rectangle(r: &RectangleState) -> DumpRectangleState {
    DumpRectangleState {
        killed: r.killed.clone(),
    }
}

pub(crate) fn dump_kmacro(km: &KmacroManager) -> DumpKmacroManager {
    DumpKmacroManager {
        // Live recording/playback state is keyboard-runtime owned and is not
        // persisted in fresh dumps. Keep the fields for backward-compatible
        // decoding of older pdumps only.
        current_macro: Vec::new(),
        last_macro: None,
        macro_ring: km
            .macro_ring
            .iter()
            .map(|m| m.iter().map(dump_value).collect())
            .collect(),
        counter: km.counter,
        counter_format: km.counter_format.clone(),
    }
}

pub(crate) fn dump_register_manager(rm: &RegisterManager) -> DumpRegisterManager {
    DumpRegisterManager {
        registers: rm
            .dump_registers()
            .iter()
            .map(|(c, r)| {
                (
                    *c,
                    match r {
                        RegisterContent::Text(s) => DumpRegisterContent::Text(s.clone()),
                        RegisterContent::Number(n) => DumpRegisterContent::Number(*n),
                        RegisterContent::Position { buffer, point } => {
                            DumpRegisterContent::Position {
                                buffer: buffer.clone(),
                                point: *point,
                            }
                        }
                        RegisterContent::Rectangle(lines) => {
                            DumpRegisterContent::Rectangle(lines.clone())
                        }
                        RegisterContent::FrameConfig(v) => {
                            DumpRegisterContent::FrameConfig(dump_value(v))
                        }
                        RegisterContent::File(s) => DumpRegisterContent::File(s.clone()),
                        RegisterContent::KbdMacro(keys) => {
                            DumpRegisterContent::KbdMacro(keys.iter().map(dump_value).collect())
                        }
                    },
                )
            })
            .collect(),
    }
}

pub(crate) fn dump_bookmark_manager(bm: &BookmarkManager) -> DumpBookmarkManager {
    DumpBookmarkManager {
        bookmarks: bm
            .dump_bookmarks()
            .iter()
            .map(|(k, b)| {
                (
                    k.clone(),
                    DumpBookmark {
                        name: b.name.clone(),
                        filename: b.filename.clone(),
                        position: b.position,
                        front_context: b.front_context.clone(),
                        rear_context: b.rear_context.clone(),
                        annotation: b.annotation.clone(),
                        handler: b.handler.clone(),
                    },
                )
            })
            .collect(),
        recent: bm.dump_recent().to_vec(),
    }
}

pub(crate) fn dump_abbrev_manager(am: &AbbrevManager) -> DumpAbbrevManager {
    DumpAbbrevManager {
        tables: am
            .dump_tables()
            .iter()
            .map(|(k, t)| {
                (
                    k.clone(),
                    DumpAbbrevTable {
                        name: t.name.clone(),
                        abbrevs: t
                            .abbrevs
                            .iter()
                            .map(|(k, a)| {
                                (
                                    k.clone(),
                                    DumpAbbrev {
                                        expansion: a.expansion.clone(),
                                        hook: a.hook.clone(),
                                        count: a.count,
                                        system: a.system,
                                    },
                                )
                            })
                            .collect(),
                        parent: t.parent.clone(),
                        case_fixed: t.case_fixed,
                        enable_quoting: t.enable_quoting,
                    },
                )
            })
            .collect(),
        global_table_name: am.dump_global_table_name().to_owned(),
        abbrev_mode: am.dump_abbrev_mode(),
    }
}

pub(crate) fn dump_interactive_registry(ir: &InteractiveRegistry) -> DumpInteractiveRegistry {
    DumpInteractiveRegistry {
        specs: ir
            .dump_specs()
            .iter()
            .map(|(k, s)| {
                (
                    k.clone(),
                    DumpInteractiveSpec {
                        code: s.code.clone(),
                        prompt: s.prompt.clone(),
                    },
                )
            })
            .collect(),
    }
}

pub(crate) fn dump_watcher_list(wl: &VariableWatcherList) -> DumpVariableWatcherList {
    DumpVariableWatcherList {
        watchers: wl
            .dump_watchers()
            .iter()
            .map(|(k, watchers)| {
                (
                    k.clone(),
                    watchers.iter().map(|w| dump_value(&w.callback)).collect(),
                )
            })
            .collect(),
    }
}

pub(crate) fn dump_string_text_prop_run(r: &StringTextPropertyRun) -> DumpStringTextPropertyRun {
    DumpStringTextPropertyRun {
        start: r.start,
        end: r.end,
        plist: dump_value(&r.plist),
    }
}

/// Convert a TextPropertyTable to a list of DumpPropertyInterval entries (for string props).
/// Does NOT allocate heap objects — serializes property values directly.
fn dump_string_text_property_table(table: &TextPropertyTable) -> Vec<DumpPropertyInterval> {
    let mut intervals = Vec::new();
    for iv in table.dump_intervals() {
        if iv.properties.is_empty() {
            continue;
        }
        let properties: Vec<(String, DumpValue)> = iv
            .properties
            .iter()
            .map(|(key, val)| (key.clone(), dump_value(val)))
            .collect();
        intervals.push(DumpPropertyInterval {
            start: iv.start,
            end: iv.end,
            properties,
        });
    }
    intervals
}

// --- Top-level dump ---

pub(crate) fn dump_evaluator(eval: &Context) -> DumpContextState {
    let mut dump_state = TaggedDumpState::new();
    PDUMP_DUMP_STATE.with(|state| state.set(&mut dump_state));

    let dump = DumpContextState {
        interner: dump_interner(),
        heap: DumpLispHeap {
            objects: Vec::new(),
        },
        obarray: dump_obarray(&eval.obarray),
        dynamic: Vec::new(),
        lexenv: dump_value(&eval.lexenv),
        features: eval.features.iter().map(|s| s.0).collect(),
        require_stack: eval.require_stack.iter().map(|s| s.0).collect(),
        buffers: dump_buffer_manager(&eval.buffers),
        autoloads: dump_autoload_manager(&eval.autoloads),
        custom: dump_custom_manager(&eval.custom),
        modes: dump_mode_registry(&eval.modes),
        coding_systems: dump_coding_system_manager(&eval.coding_systems),
        charset_registry: dump_charset_registry(),
        fontset_registry: dump_fontset_registry(),
        face_table: dump_face_table(&eval.face_table),
        abbrevs: dump_abbrev_manager(&eval.abbrevs),
        interactive: dump_interactive_registry(&eval.interactive),
        rectangle: dump_rectangle(&eval.rectangle),
        standard_syntax_table: dump_value(&eval.standard_syntax_table),
        standard_category_table: dump_value(&eval.standard_category_table),
        current_local_map: dump_value(&eval.current_local_map),
        kmacro: dump_kmacro(&eval.kmacro),
        registers: dump_register_manager(&eval.registers),
        bookmarks: dump_bookmark_manager(&eval.bookmarks),
        watchers: dump_watcher_list(&eval.watchers),
    };

    PDUMP_DUMP_STATE.with(|state| state.set(std::ptr::null_mut()));
    let heap = dump_state.finalize();

    DumpContextState { heap, ..dump }
}

// ===========================================================================
// Load direction: Dump → Runtime
// ===========================================================================

// --- Primitives ---

pub(crate) fn load_sym_id(id: &DumpSymId) -> SymId {
    SymId(id.0)
}

fn load_cached_buffer(id: u64) -> Value {
    with_load_state(|state| {
        *state
            .buffers
            .entry(id)
            .or_insert_with(|| Value::make_buffer(BufferId(id)))
    })
}

fn load_cached_window(id: u64) -> Value {
    with_load_state(|state| {
        *state
            .windows
            .entry(id)
            .or_insert_with(|| Value::make_window(id))
    })
}

fn load_cached_frame(id: u64) -> Value {
    with_load_state(|state| {
        *state
            .frames
            .entry(id)
            .or_insert_with(|| Value::make_frame(id))
    })
}

fn load_cached_timer(id: u64) -> Value {
    with_load_state(|state| {
        *state
            .timers
            .entry(id)
            .or_insert_with(|| Value::make_timer(id))
    })
}

fn allocate_tagged_placeholder(
    state: &mut TaggedLoadState,
    id: TaggedObjId,
) -> Result<Value, DumpError> {
    if let Some(value) = state.values[id.index as usize] {
        return Ok(value);
    }
    let value = match &state.objects[id.index as usize] {
        DumpHeapObject::Cons { .. } => Value::cons(Value::NIL, Value::NIL),
        DumpHeapObject::Vector(items) => Value::make_vector(vec![Value::NIL; items.len()]),
        DumpHeapObject::HashTable(ht) => with_tagged_heap(|heap| {
            heap.alloc_hash_table(LispHashTable::new_with_options(
                load_hash_table_test(&ht.test),
                ht.size,
                ht.weakness.as_ref().map(load_hash_table_weakness),
                ht.rehash_size,
                ht.rehash_threshold,
            ))
        }),
        DumpHeapObject::Str {
            text, multibyte, ..
        } => Value::heap_string(LispString::new(text.clone(), *multibyte)),
        DumpHeapObject::Float(value) => Value::make_float(*value),
        DumpHeapObject::Lambda(slots) => with_tagged_heap(|heap| {
            heap.alloc_lambda(vec![Value::NIL; slots.len().max(CLOSURE_MIN_SLOTS)])
        }),
        DumpHeapObject::Macro(slots) => with_tagged_heap(|heap| {
            heap.alloc_macro(vec![Value::NIL; slots.len().max(CLOSURE_MIN_SLOTS)])
        }),
        DumpHeapObject::ByteCode(_) => Value::make_bytecode(ByteCodeFunction {
            ops: Vec::new(),
            constants: Vec::new(),
            max_stack: 0,
            params: LambdaParams::simple(Vec::new()),
            lexical: false,
            env: None,
            gnu_byte_offset_map: None,
            docstring: None,
            doc_form: None,
            interactive: None,
        }),
        DumpHeapObject::Record(items) => Value::make_record(vec![Value::NIL; items.len()]),
        DumpHeapObject::Marker(marker) => Value::make_marker(crate::heap_types::MarkerData {
            buffer: marker.buffer.map(|id| BufferId(id.0)),
            position: marker.position,
            insertion_type: marker.insertion_type,
            marker_id: marker.marker_id,
        }),
        DumpHeapObject::Overlay(overlay) => Value::make_overlay(crate::heap_types::OverlayData {
            plist: Value::NIL,
            buffer: overlay.buffer.map(|id| BufferId(id.0)),
            start: overlay.start,
            end: overlay.end,
            front_advance: overlay.front_advance,
            rear_advance: overlay.rear_advance,
        }),
        DumpHeapObject::Buffer(id) => load_cached_buffer(id.0),
        DumpHeapObject::Window(id) => load_cached_window(*id),
        DumpHeapObject::Frame(id) => load_cached_frame(*id),
        DumpHeapObject::Timer(id) => load_cached_timer(*id),
        DumpHeapObject::Subr { name, .. } => Value::subr(load_sym_id(name)),
        DumpHeapObject::Free => Value::NIL,
    };
    state.values[id.index as usize] = Some(value);
    Ok(value)
}

fn populate_tagged_object(state: &mut TaggedLoadState, id: TaggedObjId) -> Result<(), DumpError> {
    if state.populated[id.index as usize] {
        return Ok(());
    }

    let value = allocate_tagged_placeholder(state, id)?;
    state.populated[id.index as usize] = true;
    match state.objects[id.index as usize].clone() {
        DumpHeapObject::Cons { car, cdr } => {
            value.set_car(load_value(&car));
            value.set_cdr(load_value(&cdr));
        }
        DumpHeapObject::Vector(items) => {
            let _ = value.with_vector_data_mut(|data| {
                data.clear();
                data.extend(items.iter().map(load_value));
            });
        }
        DumpHeapObject::HashTable(ht) => {
            if let Some(table) = value.as_hash_table_mut() {
                table.test = load_hash_table_test(&ht.test);
                table.test_name = ht.test_name.map(|s| load_sym_id(&s));
                table.size = ht.size;
                table.weakness = ht.weakness.as_ref().map(load_hash_table_weakness);
                table.rehash_size = ht.rehash_size;
                table.rehash_threshold = ht.rehash_threshold;
                table.data = ht
                    .entries
                    .iter()
                    .map(|(k, v)| (load_hash_key(k), load_value(v)))
                    .collect();
                table.key_snapshots = ht
                    .key_snapshots
                    .iter()
                    .map(|(k, v)| (load_hash_key(k), load_value(v)))
                    .collect();
                table.insertion_order = ht.insertion_order.iter().map(load_hash_key).collect();
            }
        }
        DumpHeapObject::Str { text_props, .. } => {
            if !text_props.is_empty() {
                let runs = text_props
                    .iter()
                    .map(|run| StringTextPropertyRun {
                        start: run.start,
                        end: run.end,
                        plist: load_value(&run.plist),
                    })
                    .collect();
                set_string_text_properties_for_value(value, runs);
            }
        }
        DumpHeapObject::Float(_) => {}
        DumpHeapObject::Lambda(slots) | DumpHeapObject::Macro(slots) => {
            let _ = value.with_closure_slots_mut(|data| {
                data.clear();
                data.extend(slots.iter().map(load_value));
            });
        }
        DumpHeapObject::ByteCode(bc) => {
            if let Some(data) = value.get_bytecode_data_mut() {
                *data = load_bytecode(&bc)?;
            }
        }
        DumpHeapObject::Record(items) => {
            let _ = value.with_record_data_mut(|data| {
                data.clear();
                data.extend(items.iter().map(load_value));
            });
        }
        DumpHeapObject::Marker(marker) => {
            if let Some(data) = value.as_marker_data_mut() {
                data.buffer = marker.buffer.map(|id| BufferId(id.0));
                data.position = marker.position;
                data.insertion_type = marker.insertion_type;
                data.marker_id = marker.marker_id;
            }
        }
        DumpHeapObject::Overlay(overlay) => {
            if let Some(data) = value.as_overlay_data_mut() {
                data.plist = load_value(&overlay.plist);
                data.buffer = overlay.buffer.map(|id| BufferId(id.0));
                data.start = overlay.start;
                data.end = overlay.end;
                data.front_advance = overlay.front_advance;
                data.rear_advance = overlay.rear_advance;
            }
        }
        DumpHeapObject::Buffer(_)
        | DumpHeapObject::Window(_)
        | DumpHeapObject::Frame(_)
        | DumpHeapObject::Timer(_)
        | DumpHeapObject::Subr { .. }
        | DumpHeapObject::Free => {}
    }
    Ok(())
}

fn load_tagged_object(state: &mut TaggedLoadState, id: TaggedObjId) -> Value {
    allocate_tagged_placeholder(state, id).expect("pdump placeholder allocation should succeed");
    populate_tagged_object(state, id).expect("pdump object population should succeed");
    state.values[id.index as usize].expect("pdump object should exist")
}

pub(crate) fn preload_tagged_heap(heap: &DumpLispHeap) -> Result<(), DumpError> {
    let mut load_state = Box::new(TaggedLoadState::new(heap));
    let ptr: *mut TaggedLoadState = &mut *load_state;
    PDUMP_LOAD_STATE.with(|state| state.set(ptr));
    for index in 0..load_state.objects.len() {
        if let Err(err) = populate_tagged_object(
            &mut load_state,
            TaggedObjId {
                index: index as u32,
            },
        ) {
            PDUMP_LOAD_STATE.with(|state| state.set(std::ptr::null_mut()));
            return Err(err);
        }
    }
    std::mem::forget(load_state);
    Ok(())
}

pub(crate) fn finish_preload_tagged_heap() {
    PDUMP_LOAD_STATE.with(|state| {
        let ptr = state.replace(std::ptr::null_mut());
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    });
}

pub(crate) fn load_value(v: &DumpValue) -> Value {
    match v {
        DumpValue::Nil => Value::NIL,
        DumpValue::True => Value::T,
        DumpValue::Int(n) => Value::fixnum(*n),
        DumpValue::Float(f, id) => load_float_value(*id, *f),
        DumpValue::Symbol(s) => Value::symbol(load_sym_id(s)),
        DumpValue::Str(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Cons(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Vector(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Record(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::HashTable(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Lambda(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Macro(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Subr(s) => Value::subr(load_sym_id(s)),
        DumpValue::ByteCode(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Marker(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Overlay(id) => obj_id_to_value(tagged_obj_id(id)),
        DumpValue::Buffer(bid) => load_cached_buffer(bid.0),
        DumpValue::Window(w) => load_cached_window(*w),
        DumpValue::Frame(f) => load_cached_frame(*f),
        DumpValue::Timer(t) => load_cached_timer(*t),
    }
}

pub(crate) fn load_opt_value(v: &Option<DumpValue>) -> Option<Value> {
    v.as_ref().map(load_value)
}

// --- Expr ---

pub(crate) fn load_expr(e: &DumpExpr) -> Expr {
    match e {
        DumpExpr::Int(n) => Expr::Int(*n),
        DumpExpr::Float(f) => Expr::Float(*f),
        DumpExpr::Symbol(s) => Expr::Symbol(load_sym_id(s)),
        DumpExpr::ReaderLoadFileName => Expr::ReaderLoadFileName,
        DumpExpr::Keyword(s) => Expr::Keyword(load_sym_id(s)),
        DumpExpr::Str(s) => Expr::Str(s.clone()),
        DumpExpr::Char(c) => Expr::Char(*c),
        DumpExpr::List(items) => Expr::List(items.iter().map(load_expr).collect()),
        DumpExpr::Vector(items) => Expr::Vector(items.iter().map(load_expr).collect()),
        DumpExpr::DottedList(items, tail) => Expr::DottedList(
            items.iter().map(load_expr).collect(),
            Box::new(load_expr(tail)),
        ),
        DumpExpr::Bool(b) => Expr::Bool(*b),
        DumpExpr::OpaqueValue(v) => {
            let val = load_value(v);
            Expr::OpaqueValueRef(
                crate::emacs_core::eval::OPAQUE_POOL.with(|pool| pool.borrow_mut().insert(val)),
            )
        }
    }
}

// --- Op ---

pub(crate) fn load_op(op: &DumpOp) -> Result<Op, DumpError> {
    let op = match *op {
        DumpOp::Constant(n) => Op::Constant(n),
        DumpOp::Nil => Op::Nil,
        DumpOp::True => Op::True,
        DumpOp::Pop => Op::Pop,
        DumpOp::Dup => Op::Dup,
        DumpOp::StackRef(n) => Op::StackRef(n),
        DumpOp::StackSet(n) => Op::StackSet(n),
        DumpOp::DiscardN(n) => Op::DiscardN(n),
        DumpOp::VarRef(n) => Op::VarRef(n),
        DumpOp::VarSet(n) => Op::VarSet(n),
        DumpOp::VarBind(n) => Op::VarBind(n),
        DumpOp::Unbind(n) => Op::Unbind(n),
        DumpOp::Call(n) => Op::Call(n),
        DumpOp::Apply(n) => Op::Apply(n),
        DumpOp::Goto(n) => Op::Goto(n),
        DumpOp::GotoIfNil(n) => Op::GotoIfNil(n),
        DumpOp::GotoIfNotNil(n) => Op::GotoIfNotNil(n),
        DumpOp::GotoIfNilElsePop(n) => Op::GotoIfNilElsePop(n),
        DumpOp::GotoIfNotNilElsePop(n) => Op::GotoIfNotNilElsePop(n),
        DumpOp::Switch => Op::Switch,
        DumpOp::Return => Op::Return,
        DumpOp::Add => Op::Add,
        DumpOp::Sub => Op::Sub,
        DumpOp::Mul => Op::Mul,
        DumpOp::Div => Op::Div,
        DumpOp::Rem => Op::Rem,
        DumpOp::Add1 => Op::Add1,
        DumpOp::Sub1 => Op::Sub1,
        DumpOp::Negate => Op::Negate,
        DumpOp::Eqlsign => Op::Eqlsign,
        DumpOp::Gtr => Op::Gtr,
        DumpOp::Lss => Op::Lss,
        DumpOp::Leq => Op::Leq,
        DumpOp::Geq => Op::Geq,
        DumpOp::Max => Op::Max,
        DumpOp::Min => Op::Min,
        DumpOp::Car => Op::Car,
        DumpOp::Cdr => Op::Cdr,
        DumpOp::Cons => Op::Cons,
        DumpOp::List(n) => Op::List(n),
        DumpOp::Length => Op::Length,
        DumpOp::Nth => Op::Nth,
        DumpOp::Nthcdr => Op::Nthcdr,
        DumpOp::Setcar => Op::Setcar,
        DumpOp::Setcdr => Op::Setcdr,
        DumpOp::CarSafe => Op::CarSafe,
        DumpOp::CdrSafe => Op::CdrSafe,
        DumpOp::Elt => Op::Elt,
        DumpOp::Nconc => Op::Nconc,
        DumpOp::Nreverse => Op::Nreverse,
        DumpOp::Member => Op::Member,
        DumpOp::Memq => Op::Memq,
        DumpOp::Assq => Op::Assq,
        DumpOp::Symbolp => Op::Symbolp,
        DumpOp::Consp => Op::Consp,
        DumpOp::Stringp => Op::Stringp,
        DumpOp::Listp => Op::Listp,
        DumpOp::Integerp => Op::Integerp,
        DumpOp::Numberp => Op::Numberp,
        DumpOp::Null => Op::Null,
        DumpOp::Not => Op::Not,
        DumpOp::Eq => Op::Eq,
        DumpOp::Equal => Op::Equal,
        DumpOp::Concat(n) => Op::Concat(n),
        DumpOp::Substring => Op::Substring,
        DumpOp::StringEqual => Op::StringEqual,
        DumpOp::StringLessp => Op::StringLessp,
        DumpOp::Aref => Op::Aref,
        DumpOp::Aset => Op::Aset,
        DumpOp::SymbolValue => Op::SymbolValue,
        DumpOp::SymbolFunction => Op::SymbolFunction,
        DumpOp::Set => Op::Set,
        DumpOp::Fset => Op::Fset,
        DumpOp::Get => Op::Get,
        DumpOp::Put => Op::Put,
        DumpOp::PushConditionCase(n) => Op::PushConditionCase(n),
        DumpOp::PushConditionCaseRaw(n) => Op::PushConditionCaseRaw(n),
        DumpOp::PushCatch(n) => Op::PushCatch(n),
        DumpOp::PopHandler => Op::PopHandler,
        DumpOp::UnwindProtect(n) => {
            return Err(DumpError::DeserializationError(format!(
                "legacy neomacs unwind-protect opcode is unsupported in pdump snapshots; rebuild the dump or recompile this bytecode (target {n})"
            )));
        }
        DumpOp::UnwindProtectPop => Op::UnwindProtectPop,
        DumpOp::Throw => Op::Throw,
        DumpOp::SaveCurrentBuffer => Op::SaveCurrentBuffer,
        DumpOp::SaveExcursion => Op::SaveExcursion,
        DumpOp::SaveRestriction => Op::SaveRestriction,
        DumpOp::MakeClosure(n) => Op::MakeClosure(n),
        DumpOp::CallBuiltin(a, b) => Op::CallBuiltin(a, b),
    };
    Ok(op)
}

// --- Lambda / ByteCode ---

pub(crate) fn load_lambda_params(p: &DumpLambdaParams) -> LambdaParams {
    LambdaParams {
        required: p.required.iter().map(|s| load_sym_id(s)).collect(),
        optional: p.optional.iter().map(|s| load_sym_id(s)).collect(),
        rest: p.rest.map(|s| load_sym_id(&s)),
    }
}

pub(crate) fn load_bytecode(bc: &DumpByteCodeFunction) -> Result<ByteCodeFunction, DumpError> {
    Ok(ByteCodeFunction {
        ops: bc.ops.iter().map(load_op).collect::<Result<Vec<_>, _>>()?,
        constants: bc.constants.iter().map(load_value).collect(),
        max_stack: bc.max_stack,
        params: load_lambda_params(&bc.params),
        lexical: bc.lexical,
        env: load_opt_value(&bc.env),
        gnu_byte_offset_map: bc.gnu_byte_offset_map.as_ref().map(|pairs| {
            pairs
                .iter()
                .map(|(byte_off, instr_idx)| (*byte_off as usize, *instr_idx as usize))
                .collect()
        }),
        docstring: bc.docstring.clone(),
        doc_form: load_opt_value(&bc.doc_form),
        interactive: load_opt_value(&bc.interactive),
    })
}

// --- Hash tables ---

pub(crate) fn load_hash_key(k: &DumpHashKey) -> HashKey {
    match k {
        DumpHashKey::Nil => HashKey::Nil,
        DumpHashKey::True => HashKey::True,
        DumpHashKey::Int(n) => HashKey::Int(*n),
        DumpHashKey::Float(bits) => HashKey::Float(*bits),
        DumpHashKey::FloatEq(bits, id) => HashKey::FloatEq(*bits, *id),
        DumpHashKey::Symbol(s) => HashKey::Symbol(load_sym_id(s)),
        DumpHashKey::Keyword(s) => HashKey::Keyword(load_sym_id(s)),
        DumpHashKey::Str(id) => HashKey::Ptr(obj_id_to_value(tagged_obj_id(id)).bits()),
        DumpHashKey::Char(c) => HashKey::Char(*c),
        DumpHashKey::Window(w) => HashKey::Window(*w),
        DumpHashKey::Frame(f) => HashKey::Frame(*f),
        DumpHashKey::Ptr(p) => HashKey::Ptr(*p as usize),
        DumpHashKey::ObjId(a) => HashKey::Ptr(obj_id_to_value(TaggedObjId { index: *a }).bits()),
        DumpHashKey::EqualCons(a, b) => {
            HashKey::EqualCons(Box::new(load_hash_key(a)), Box::new(load_hash_key(b)))
        }
        DumpHashKey::EqualVec(v) => HashKey::EqualVec(v.iter().map(load_hash_key).collect()),
        DumpHashKey::Cycle(index) => HashKey::Cycle(*index),
        DumpHashKey::Text(text) => HashKey::Text(text.clone()),
    }
}

pub(crate) fn load_hash_table_test(t: &DumpHashTableTest) -> HashTableTest {
    match t {
        DumpHashTableTest::Eq => HashTableTest::Eq,
        DumpHashTableTest::Eql => HashTableTest::Eql,
        DumpHashTableTest::Equal => HashTableTest::Equal,
    }
}

pub(crate) fn load_hash_table_weakness(w: &DumpHashTableWeakness) -> HashTableWeakness {
    match w {
        DumpHashTableWeakness::Key => HashTableWeakness::Key,
        DumpHashTableWeakness::Value => HashTableWeakness::Value,
        DumpHashTableWeakness::KeyOrValue => HashTableWeakness::KeyOrValue,
        DumpHashTableWeakness::KeyAndValue => HashTableWeakness::KeyAndValue,
    }
}

pub(crate) fn load_hash_table(ht: &DumpLispHashTable) -> LispHashTable {
    let data: HashMap<HashKey, Value> = ht
        .entries
        .iter()
        .map(|(k, v)| (load_hash_key(k), load_value(v)))
        .collect();
    let key_snapshots: HashMap<HashKey, Value> = ht
        .key_snapshots
        .iter()
        .map(|(k, v)| (load_hash_key(k), load_value(v)))
        .collect();
    let insertion_order: Vec<HashKey> = ht.insertion_order.iter().map(load_hash_key).collect();

    LispHashTable {
        test: load_hash_table_test(&ht.test),
        test_name: ht.test_name.map(|s| load_sym_id(&s)),
        size: ht.size,
        weakness: ht.weakness.as_ref().map(load_hash_table_weakness),
        rehash_size: ht.rehash_size,
        rehash_threshold: ht.rehash_threshold,
        data,
        key_snapshots,
        insertion_order,
    }
}

// --- Interner ---

pub(crate) fn load_interner(di: &DumpStringInterner) {
    intern::ensure_runtime_interner(&di.strings);
}

// --- Symbol / Obarray ---

fn load_symbol_value_enum(dsv: &DumpSymbolValue) -> SymbolValue {
    match dsv {
        DumpSymbolValue::Plain(v) => SymbolValue::Plain(load_opt_value(v)),
        DumpSymbolValue::Alias(target) => SymbolValue::Alias(load_sym_id(target)),
        DumpSymbolValue::BufferLocal {
            default,
            local_if_set,
        } => SymbolValue::BufferLocal {
            default: load_opt_value(default),
            local_if_set: *local_if_set,
        },
        DumpSymbolValue::Forwarded => SymbolValue::Forwarded,
    }
}

pub(crate) fn load_symbol_data(sd: &DumpSymbolData) -> SymbolData {
    // Prefer the new `symbol_value` field; fall back to legacy `value` field
    // for backward compatibility with older pdump files.
    let value = if let Some(ref sv) = sd.symbol_value {
        load_symbol_value_enum(sv)
    } else {
        SymbolValue::Plain(load_opt_value(&sd.value))
    };
    SymbolData {
        name: load_sym_id(&sd.name),
        value,
        function: load_opt_value(&sd.function),
        plist: sd
            .plist
            .iter()
            .map(|(k, v)| (load_sym_id(k), load_value(v)))
            .collect(),
        special: sd.special,
        constant: sd.constant,
    }
}

pub(crate) fn load_obarray(dob: &DumpObarray) -> Obarray {
    let symbols: HashMap<SymId, SymbolData> = dob
        .symbols
        .iter()
        .map(|(id, sd)| (SymId(*id), load_symbol_data(sd)))
        .collect();
    let global_members: HashSet<SymId> = dob.global_members.iter().map(|id| SymId(*id)).collect();
    let function_unbound: HashSet<SymId> =
        dob.function_unbound.iter().map(|id| SymId(*id)).collect();
    Obarray::from_dump(
        symbols,
        global_members,
        function_unbound,
        dob.function_epoch,
    )
}

// --- Buffer types ---

fn load_insertion_type(it: &DumpInsertionType) -> InsertionType {
    match it {
        DumpInsertionType::Before => InsertionType::Before,
        DumpInsertionType::After => InsertionType::After,
    }
}

fn load_marker(m: &DumpMarkerEntry, text: &BufferText) -> MarkerEntry {
    MarkerEntry {
        id: m.id,
        buffer_id: BufferId(m.buffer_id),
        byte_pos: m.byte_pos,
        char_pos: m.char_pos.unwrap_or_else(|| text.byte_to_char(m.byte_pos)),
        insertion_type: load_insertion_type(&m.insertion_type),
    }
}

fn load_property_interval(pi: &DumpPropertyInterval) -> PropertyInterval {
    let properties: std::collections::HashMap<String, crate::emacs_core::value::Value> = pi
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), load_value(v)))
        .collect();
    let key_order: Vec<String> = pi.properties.iter().map(|(k, _)| k.clone()).collect();
    PropertyInterval {
        start: pi.start,
        end: pi.end,
        properties,
        key_order,
    }
}

fn load_syntax_class(c: &DumpSyntaxClass) -> SyntaxClass {
    match c {
        DumpSyntaxClass::Whitespace => SyntaxClass::Whitespace,
        DumpSyntaxClass::Word => SyntaxClass::Word,
        DumpSyntaxClass::Symbol => SyntaxClass::Symbol,
        DumpSyntaxClass::Punctuation => SyntaxClass::Punctuation,
        DumpSyntaxClass::Open => SyntaxClass::Open,
        DumpSyntaxClass::Close => SyntaxClass::Close,
        DumpSyntaxClass::Prefix => SyntaxClass::Quote,
        DumpSyntaxClass::StringDelim => SyntaxClass::StringDelim,
        DumpSyntaxClass::MathDelim => SyntaxClass::Math,
        DumpSyntaxClass::Escape => SyntaxClass::Escape,
        DumpSyntaxClass::CharQuote => SyntaxClass::CharQuote,
        DumpSyntaxClass::Comment => SyntaxClass::Comment,
        DumpSyntaxClass::EndComment => SyntaxClass::EndComment,
        DumpSyntaxClass::InheritStandard => SyntaxClass::InheritStd,
        DumpSyntaxClass::Generic => SyntaxClass::CommentFence,
        DumpSyntaxClass::StringFence => SyntaxClass::StringFence,
    }
}

fn load_syntax_entry(se: &DumpSyntaxEntry) -> SyntaxEntry {
    SyntaxEntry {
        class: load_syntax_class(&se.class),
        matching_char: se.matching_char,
        flags: SyntaxFlags::new(se.flags),
    }
}

fn load_syntax_table(st: &DumpSyntaxTable) -> SyntaxTable {
    let entries: HashMap<char, SyntaxEntry> = st
        .entries
        .iter()
        .map(|(c, e)| (*c, load_syntax_entry(e)))
        .collect();
    let parent = st.parent.as_ref().map(|p| Box::new(load_syntax_table(p)));
    SyntaxTable::from_dump(entries, parent)
}

// load_undo_record removed — undo state is loaded from buffer-local properties.

fn load_buffer(db: &DumpBuffer) -> Buffer {
    let text = BufferText::from_dump(db.text.text.clone());
    let total_chars = text.char_count();
    let begv_char = db.begv_char.unwrap_or_else(|| text.byte_to_char(db.begv));
    let zv_char = db.zv_char.unwrap_or_else(|| {
        if db.zv == text.len() {
            total_chars
        } else {
            text.byte_to_char(db.zv)
        }
    });
    let pt_char = db.pt_char.unwrap_or_else(|| {
        if db.pt == db.begv {
            begv_char
        } else if db.pt == db.zv {
            zv_char
        } else {
            text.byte_to_char(db.pt)
        }
    });
    let mark_char = match db.mark {
        Some(mark) => Some(db.mark_char.unwrap_or_else(|| {
            if mark == db.begv {
                begv_char
            } else if mark == db.zv {
                zv_char
            } else {
                text.byte_to_char(mark)
            }
        })),
        None => None,
    };
    for marker in db.markers.iter().map(|marker| load_marker(marker, &text)) {
        text.register_marker(
            marker.buffer_id,
            marker.id,
            marker.byte_pos,
            marker.char_pos,
            marker.insertion_type,
        );
    }
    let locals = crate::buffer::BufferLocals::from_dump(
        db.properties
            .iter()
            .map(|(k, v)| (k.clone(), load_runtime_binding_value(v)))
            .collect(),
        &db.local_binding_names,
        load_value(&db.local_map),
    );
    let undo_list = match locals.raw_binding("buffer-undo-list") {
        Some(RuntimeBindingValue::Bound(value)) => value,
        _ => Value::NIL,
    };

    let save_modified_tick = db.save_modified_tick.unwrap_or_else(|| {
        if db.modified {
            db.modified_tick.saturating_sub(1)
        } else {
            db.modified_tick
        }
    });
    let autosave_modified_tick = db.autosave_modified_tick.unwrap_or(save_modified_tick);
    let last_window_start = db.last_window_start.unwrap_or(1).max(1);

    let text_props = TextPropertyTable::from_dump(
        db.text_props
            .intervals
            .iter()
            .map(load_property_interval)
            .collect(),
    );
    text.text_props_replace(text_props);

    Buffer {
        id: BufferId(db.id.0),
        name: db.name.clone(),
        base_buffer: db.base_buffer.map(|id| BufferId(id.0)),
        text,
        pt: db.pt,
        pt_char,
        mark: db.mark,
        mark_char,
        begv: db.begv,
        begv_char,
        zv: db.zv,
        zv_char,
        modified: db.modified,
        modified_tick: db.modified_tick,
        chars_modified_tick: db.chars_modified_tick,
        save_modified_tick,
        autosave_modified_tick,
        last_window_start,
        last_selected_window: None,
        inhibit_buffer_hooks: false,
        read_only: db.read_only,
        multibyte: db.multibyte,
        file_name: db.file_name.clone(),
        auto_save_file_name: db.auto_save_file_name.clone(),
        state_markers: match (db.state_pt_marker, db.state_begv_marker, db.state_zv_marker) {
            (Some(pt_marker), Some(begv_marker), Some(zv_marker)) => {
                Some(crate::buffer::buffer::BufferStateMarkers {
                    pt_marker,
                    begv_marker,
                    zv_marker,
                })
            }
            _ => None,
        },
        locals,
        overlays: OverlayList::from_dump(
            db.overlays
                .overlays
                .iter()
                .map(|d| {
                    Value::make_overlay(crate::heap_types::OverlayData {
                        plist: load_value(&d.plist),
                        buffer: d.buffer.map(|id| BufferId(id.0)),
                        start: d.start,
                        end: d.end,
                        front_advance: d.front_advance,
                        rear_advance: d.rear_advance,
                    })
                })
                .collect(),
        ),
        syntax_table: load_syntax_table(&db.syntax_table),
        undo_state: SharedUndoState::from_parts(undo_list, false, false),
    }
}

pub(crate) fn load_buffer_manager(dbm: &DumpBufferManager) -> BufferManager {
    let buffers: HashMap<BufferId, Buffer> = dbm
        .buffers
        .iter()
        .map(|(id, buf)| (BufferId(id.0), load_buffer(buf)))
        .collect();
    BufferManager::from_dump(
        buffers,
        dbm.current.map(|id| BufferId(id.0)),
        dbm.next_id,
        dbm.next_marker_id,
    )
}

// --- Sub-managers ---

pub(crate) fn load_autoload_manager(dam: &DumpAutoloadManager) -> AutoloadManager {
    let entries: HashMap<String, AutoloadEntry> = dam
        .entries
        .iter()
        .map(|(k, e)| {
            (
                k.clone(),
                AutoloadEntry {
                    name: e.name.clone(),
                    file: e.file.clone(),
                    docstring: e.docstring.clone(),
                    interactive: e.interactive,
                    autoload_type: match e.autoload_type {
                        DumpAutoloadType::Function => AutoloadType::Function,
                        DumpAutoloadType::Macro => AutoloadType::Macro,
                        DumpAutoloadType::Keymap => AutoloadType::Keymap,
                    },
                },
            )
        })
        .collect();
    let after_load: HashMap<String, Vec<Value>> = dam
        .after_load
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(load_value).collect()))
        .collect();
    AutoloadManager::from_dump(
        entries,
        after_load,
        dam.loaded_files.clone(),
        dam.obsolete_functions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        dam.obsolete_variables
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

pub(crate) fn load_custom_manager(dcm: &DumpCustomManager) -> CustomManager {
    CustomManager {
        auto_buffer_local: dcm.auto_buffer_local.iter().cloned().collect(),
    }
}

fn load_mode_custom_type(ct: &DumpModeCustomType) -> ModeCustomType {
    match ct {
        DumpModeCustomType::Boolean => ModeCustomType::Boolean,
        DumpModeCustomType::Integer => ModeCustomType::Integer,
        DumpModeCustomType::Float => ModeCustomType::Float,
        DumpModeCustomType::String => ModeCustomType::String,
        DumpModeCustomType::Symbol => ModeCustomType::Symbol,
        DumpModeCustomType::Sexp => ModeCustomType::Sexp,
        DumpModeCustomType::Choice(choices) => ModeCustomType::Choice(
            choices
                .iter()
                .map(|(s, v)| (s.clone(), load_value(v)))
                .collect(),
        ),
        DumpModeCustomType::List(inner) => {
            ModeCustomType::List(Box::new(load_mode_custom_type(inner)))
        }
        DumpModeCustomType::Alist(k, v) => ModeCustomType::Alist(
            Box::new(load_mode_custom_type(k)),
            Box::new(load_mode_custom_type(v)),
        ),
        DumpModeCustomType::Plist(k, v) => ModeCustomType::Plist(
            Box::new(load_mode_custom_type(k)),
            Box::new(load_mode_custom_type(v)),
        ),
        DumpModeCustomType::Color => ModeCustomType::Color,
        DumpModeCustomType::Face => ModeCustomType::Face,
        DumpModeCustomType::File => ModeCustomType::File,
        DumpModeCustomType::Directory => ModeCustomType::Directory,
        DumpModeCustomType::Function => ModeCustomType::Function,
        DumpModeCustomType::Variable => ModeCustomType::Variable,
        DumpModeCustomType::Hook => ModeCustomType::Hook,
        DumpModeCustomType::Coding => ModeCustomType::Coding,
    }
}

pub(crate) fn load_mode_registry(dmr: &DumpModeRegistry) -> ModeRegistry {
    let major_modes: HashMap<String, MajorMode> = dmr
        .major_modes
        .iter()
        .map(|(k, m)| {
            (
                k.clone(),
                MajorMode {
                    name: m.name.clone(),
                    pretty_name: m.pretty_name.clone(),
                    parent: m.parent.clone(),
                    mode_hook: m.mode_hook.clone(),
                    keymap_name: m.keymap_name.clone(),
                    syntax_table_name: m.syntax_table_name.clone(),
                    abbrev_table_name: m.abbrev_table_name.clone(),
                    font_lock: m.font_lock.as_ref().map(|fl| FontLockDefaults {
                        keywords: fl
                            .keywords
                            .iter()
                            .map(|kw| FontLockKeyword {
                                pattern: kw.pattern.clone(),
                                face: kw.face.clone(),
                                group: kw.group,
                                override_: kw.override_,
                                laxmatch: kw.laxmatch,
                            })
                            .collect(),
                        case_fold: fl.case_fold,
                        syntax_table: fl.syntax_table.clone(),
                    }),
                    body: load_opt_value(&m.body),
                },
            )
        })
        .collect();
    let minor_modes: HashMap<String, MinorMode> = dmr
        .minor_modes
        .iter()
        .map(|(k, m)| {
            (
                k.clone(),
                MinorMode {
                    name: m.name.clone(),
                    lighter: m.lighter.clone(),
                    keymap_name: m.keymap_name.clone(),
                    global: m.global,
                    body: load_opt_value(&m.body),
                },
            )
        })
        .collect();
    let custom_variables: HashMap<String, ModeCustomVariable> = dmr
        .custom_variables
        .iter()
        .map(|(k, cv)| {
            (
                k.clone(),
                ModeCustomVariable {
                    name: cv.name.clone(),
                    default_value: load_value(&cv.default_value),
                    doc: cv.doc.clone(),
                    type_: load_mode_custom_type(&cv.custom_type),
                    group: cv.group.clone(),
                    set_function: cv.set_function.clone(),
                    get_function: cv.get_function.clone(),
                    tag: cv.tag.clone(),
                },
            )
        })
        .collect();
    let custom_groups: HashMap<String, ModeCustomGroup> = dmr
        .custom_groups
        .iter()
        .map(|(k, g)| {
            (
                k.clone(),
                ModeCustomGroup {
                    name: g.name.clone(),
                    doc: g.doc.clone(),
                    parent: g.parent.clone(),
                    members: g.members.clone(),
                },
            )
        })
        .collect();
    ModeRegistry::from_dump(
        major_modes,
        minor_modes,
        dmr.buffer_major_modes
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        dmr.buffer_minor_modes
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        dmr.global_minor_modes.clone(),
        dmr.auto_mode_alist.clone(),
        custom_variables,
        custom_groups,
        dmr.fundamental_mode.clone(),
    )
}

pub(crate) fn load_coding_system_manager(dcsm: &DumpCodingSystemManager) -> CodingSystemManager {
    let systems: HashMap<String, CodingSystemInfo> = dcsm
        .systems
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                CodingSystemInfo {
                    name: v.name.clone(),
                    coding_type: v.coding_type.clone(),
                    mnemonic: v.mnemonic,
                    eol_type: match v.eol_type {
                        DumpEolType::Unix => EolType::Unix,
                        DumpEolType::Dos => EolType::Dos,
                        DumpEolType::Mac => EolType::Mac,
                        DumpEolType::Undecided => EolType::Undecided,
                    },
                    ascii_compatible_p: v.ascii_compatible_p,
                    charset_list: v.charset_list.clone(),
                    post_read_conversion: v.post_read_conversion.clone(),
                    pre_write_conversion: v.pre_write_conversion.clone(),
                    default_char: v.default_char,
                    for_unibyte: v.for_unibyte,
                    properties: v
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), load_value(v)))
                        .collect(),
                    int_properties: v
                        .int_properties
                        .iter()
                        .map(|(k, v)| (*k, load_value(v)))
                        .collect(),
                },
            )
        })
        .collect();
    CodingSystemManager::from_dump(
        systems,
        dcsm.aliases
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        dcsm.priority.clone(),
        dcsm.keyboard_coding.clone(),
        dcsm.terminal_coding.clone(),
    )
}

pub(crate) fn load_charset_registry(dcr: &DumpCharsetRegistry) {
    let snapshot = CharsetRegistrySnapshot {
        charsets: dcr
            .charsets
            .iter()
            .map(|info| CharsetInfoSnapshot {
                id: info.id,
                name: info.name.clone(),
                dimension: info.dimension,
                code_space: info.code_space,
                min_code: info.min_code,
                max_code: info.max_code,
                iso_final_char: info.iso_final_char,
                iso_revision: info.iso_revision,
                emacs_mule_id: info.emacs_mule_id,
                ascii_compatible_p: info.ascii_compatible_p,
                supplementary_p: info.supplementary_p,
                invalid_code: info.invalid_code,
                unify_map: info.unify_map.clone(),
                method: match &info.method {
                    DumpCharsetMethod::Offset(offset) => CharsetMethodSnapshot::Offset(*offset),
                    DumpCharsetMethod::Map(map_name) => {
                        CharsetMethodSnapshot::Map(map_name.clone())
                    }
                    DumpCharsetMethod::Subset(subset) => CharsetMethodSnapshot::Subset(
                        crate::emacs_core::charset::CharsetSubsetSpec {
                            parent: subset.parent.clone(),
                            parent_min_code: subset.parent_min_code,
                            parent_max_code: subset.parent_max_code,
                            offset: subset.offset,
                        },
                    ),
                    DumpCharsetMethod::Superset(members) => {
                        CharsetMethodSnapshot::Superset(members.clone())
                    }
                },
                plist: info
                    .plist
                    .iter()
                    .map(|(key, value)| (key.clone(), load_value(value)))
                    .collect(),
            })
            .collect(),
        priority: dcr.priority.clone(),
        next_id: dcr.next_id,
    };
    restore_charset_registry(snapshot);
}

fn load_font_width(width: &DumpFontWidth) -> FontWidth {
    match width {
        DumpFontWidth::UltraCondensed => FontWidth::UltraCondensed,
        DumpFontWidth::ExtraCondensed => FontWidth::ExtraCondensed,
        DumpFontWidth::Condensed => FontWidth::Condensed,
        DumpFontWidth::SemiCondensed => FontWidth::SemiCondensed,
        DumpFontWidth::Normal => FontWidth::Normal,
        DumpFontWidth::SemiExpanded => FontWidth::SemiExpanded,
        DumpFontWidth::Expanded => FontWidth::Expanded,
        DumpFontWidth::ExtraExpanded => FontWidth::ExtraExpanded,
        DumpFontWidth::UltraExpanded => FontWidth::UltraExpanded,
    }
}

fn load_font_repertory(repertory: &DumpFontRepertory) -> FontRepertory {
    match repertory {
        DumpFontRepertory::Charset(name) => FontRepertory::Charset(name.clone()),
        DumpFontRepertory::CharTableRanges(ranges) => {
            FontRepertory::CharTableRanges(ranges.clone())
        }
    }
}

fn load_font_spec_entry(entry: &DumpFontSpecEntry) -> FontSpecEntry {
    match entry {
        DumpFontSpecEntry::Font(spec) => FontSpecEntry::Font(StoredFontSpec {
            family: spec.family.clone(),
            registry: spec.registry.clone(),
            lang: spec.lang.clone(),
            weight: spec.weight.map(FontWeight),
            slant: spec.slant.as_ref().map(load_font_slant),
            width: spec.width.as_ref().map(load_font_width),
            repertory: spec.repertory.as_ref().map(load_font_repertory),
        }),
        DumpFontSpecEntry::ExplicitNone => FontSpecEntry::ExplicitNone,
    }
}

pub(crate) fn load_fontset_registry(dfr: &DumpFontsetRegistry) {
    let snapshot = FontsetRegistrySnapshot {
        ordered_names: dfr.ordered_names.clone(),
        alias_to_name: dfr.alias_to_name.clone(),
        fontsets: dfr
            .fontsets
            .iter()
            .map(|(name, data)| {
                (
                    name.clone(),
                    FontsetDataSnapshot {
                        ranges: data
                            .ranges
                            .iter()
                            .map(|range| FontsetRangeEntrySnapshot {
                                from: range.from,
                                to: range.to,
                                entries: range.entries.iter().map(load_font_spec_entry).collect(),
                            })
                            .collect(),
                        fallback: data
                            .fallback
                            .as_ref()
                            .map(|entries| entries.iter().map(load_font_spec_entry).collect()),
                    },
                )
            })
            .collect(),
        generation: dfr.generation,
    };
    restore_fontset_registry(snapshot);
}

fn load_color(c: &DumpColor) -> Color {
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn load_font_slant(s: &DumpFontSlant) -> FontSlant {
    match s {
        DumpFontSlant::Normal => FontSlant::Normal,
        DumpFontSlant::Italic => FontSlant::Italic,
        DumpFontSlant::Oblique => FontSlant::Oblique,
        DumpFontSlant::ReverseItalic => FontSlant::ReverseItalic,
        DumpFontSlant::ReverseOblique => FontSlant::ReverseOblique,
    }
}

fn load_face(df: &DumpFace) -> Face {
    Face {
        name: df.name.clone(),
        foreground: df.foreground.map(|c| load_color(&c)),
        background: df.background.map(|c| load_color(&c)),
        family: df.family.clone(),
        height: df.height.as_ref().map(|h| match h {
            DumpFaceHeight::Absolute(n) => FaceHeight::Absolute(*n),
            DumpFaceHeight::Relative(f) => FaceHeight::Relative(*f),
        }),
        weight: df.weight.map(FontWeight),
        slant: df.slant.as_ref().map(load_font_slant),
        underline: df.underline.as_ref().map(|u| Underline {
            style: match u.style {
                DumpUnderlineStyle::Line => UnderlineStyle::Line,
                DumpUnderlineStyle::Wave => UnderlineStyle::Wave,
                DumpUnderlineStyle::Dot => UnderlineStyle::Dot,
                DumpUnderlineStyle::Dash => UnderlineStyle::Dash,
                DumpUnderlineStyle::DoubleLine => UnderlineStyle::DoubleLine,
            },
            color: u.color.map(|c| load_color(&c)),
            position: u.position,
        }),
        overline: df.overline,
        strike_through: df.strike_through,
        box_border: df.box_border.as_ref().map(|b| BoxBorder {
            color: b.color.map(|c| load_color(&c)),
            width: b.width,
            style: match b.style {
                DumpBoxStyle::Flat => BoxStyle::Flat,
                DumpBoxStyle::Raised => BoxStyle::Raised,
                DumpBoxStyle::Pressed => BoxStyle::Pressed,
            },
        }),
        inverse_video: df.inverse_video,
        stipple: df.stipple.clone(),
        extend: df.extend,
        inherit: df.inherit.clone(),
        overstrike: df.overstrike,
        doc: df.doc.clone(),
        overline_color: None,
        strike_through_color: None,
        distant_foreground: None,
        foundry: None,
        width: None,
    }
}

pub(crate) fn load_face_table(dft: &DumpFaceTable) -> FaceTable {
    FaceTable::from_dump(
        dft.faces
            .iter()
            .map(|(k, f)| (k.clone(), load_face(f)))
            .collect(),
    )
}

pub(crate) fn load_rectangle(dr: &DumpRectangleState) -> RectangleState {
    RectangleState {
        killed: dr.killed.clone(),
    }
}

pub(crate) fn load_kmacro(dkm: &DumpKmacroManager) -> KmacroManager {
    KmacroManager {
        macro_ring: dkm
            .macro_ring
            .iter()
            .map(|m| m.iter().map(load_value).collect())
            .collect(),
        counter: dkm.counter,
        counter_format: dkm.counter_format.clone(),
    }
}

pub(crate) fn load_register_manager(drm: &DumpRegisterManager) -> RegisterManager {
    let registers: HashMap<char, RegisterContent> = drm
        .registers
        .iter()
        .map(|(c, r)| {
            (
                *c,
                match r {
                    DumpRegisterContent::Text(s) => RegisterContent::Text(s.clone()),
                    DumpRegisterContent::Number(n) => RegisterContent::Number(*n),
                    DumpRegisterContent::Position { buffer, point } => RegisterContent::Position {
                        buffer: buffer.clone(),
                        point: *point,
                    },
                    DumpRegisterContent::Rectangle(lines) => {
                        RegisterContent::Rectangle(lines.clone())
                    }
                    DumpRegisterContent::FrameConfig(v) => {
                        RegisterContent::FrameConfig(load_value(v))
                    }
                    DumpRegisterContent::File(s) => RegisterContent::File(s.clone()),
                    DumpRegisterContent::KbdMacro(keys) => {
                        RegisterContent::KbdMacro(keys.iter().map(load_value).collect())
                    }
                },
            )
        })
        .collect();
    RegisterManager::from_dump(registers)
}

pub(crate) fn load_bookmark_manager(dbm: &DumpBookmarkManager) -> BookmarkManager {
    let bookmarks: HashMap<String, Bookmark> = dbm
        .bookmarks
        .iter()
        .map(|(k, b)| {
            (
                k.clone(),
                Bookmark {
                    name: b.name.clone(),
                    filename: b.filename.clone(),
                    position: b.position,
                    front_context: b.front_context.clone(),
                    rear_context: b.rear_context.clone(),
                    annotation: b.annotation.clone(),
                    handler: b.handler.clone(),
                },
            )
        })
        .collect();
    BookmarkManager::from_dump(bookmarks, dbm.recent.clone())
}

pub(crate) fn load_abbrev_manager(dam: &DumpAbbrevManager) -> AbbrevManager {
    let tables: HashMap<String, AbbrevTable> = dam
        .tables
        .iter()
        .map(|(k, t)| {
            (
                k.clone(),
                AbbrevTable {
                    name: t.name.clone(),
                    abbrevs: t
                        .abbrevs
                        .iter()
                        .map(|(k, a)| {
                            (
                                k.clone(),
                                Abbrev {
                                    expansion: a.expansion.clone(),
                                    hook: a.hook.clone(),
                                    count: a.count,
                                    system: a.system,
                                },
                            )
                        })
                        .collect(),
                    parent: t.parent.clone(),
                    case_fixed: t.case_fixed,
                    enable_quoting: t.enable_quoting,
                },
            )
        })
        .collect();
    AbbrevManager::from_dump(tables, dam.global_table_name.clone(), dam.abbrev_mode)
}

pub(crate) fn load_interactive_registry(dir: &DumpInteractiveRegistry) -> InteractiveRegistry {
    let specs: HashMap<String, InteractiveSpec> = dir
        .specs
        .iter()
        .map(|(k, s)| {
            (
                k.clone(),
                InteractiveSpec {
                    code: s.code.clone(),
                    prompt: s.prompt.clone(),
                },
            )
        })
        .collect();
    InteractiveRegistry::from_dump(specs)
}

pub(crate) fn load_watcher_list(dwl: &DumpVariableWatcherList) -> VariableWatcherList {
    let watchers: HashMap<String, Vec<VariableWatcher>> = dwl
        .watchers
        .iter()
        .map(|(k, callbacks)| {
            (
                k.clone(),
                callbacks
                    .iter()
                    .map(|v| VariableWatcher {
                        callback: load_value(v),
                    })
                    .collect(),
            )
        })
        .collect();
    VariableWatcherList::from_dump(watchers)
}

pub(crate) fn load_string_text_prop_run(r: &DumpStringTextPropertyRun) -> StringTextPropertyRun {
    StringTextPropertyRun {
        start: r.start,
        end: r.end,
        plist: load_value(&r.plist),
    }
}

/// Convert a list of DumpPropertyInterval entries back to a TextPropertyTable.
pub(crate) fn load_text_property_table(intervals: &[DumpPropertyInterval]) -> TextPropertyTable {
    let mut table = TextPropertyTable::new();
    for iv in intervals {
        for (name, dump_val) in &iv.properties {
            let val = load_value(dump_val);
            table.put_property(iv.start, iv.end, name, val);
        }
    }
    table
}
