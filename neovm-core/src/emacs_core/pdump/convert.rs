//! Conversions between runtime types and pdump snapshot types.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustc_hash::FxHashSet;

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
use crate::emacs_core::fontset::{
    FontRepertory, FontSpecEntry, FontsetDataSnapshot, FontsetRangeEntrySnapshot,
    FontsetRegistrySnapshot, StoredFontSpec, restore_fontset_registry, snapshot_fontset_registry,
};
use crate::emacs_core::interactive::{InteractiveRegistry, InteractiveSpec};
use crate::emacs_core::intern::{self, NameId, SymId};
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
    static PDUMP_LOAD_NAME_REMAP: RefCell<Option<Vec<NameId>>> = const { RefCell::new(None) };
    static PDUMP_LOAD_SYM_REMAP: RefCell<Option<Vec<SymId>>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TaggedHeapRef {
    index: u32,
}

struct TaggedDumpState {
    objects: Vec<Option<DumpHeapObject>>,
    object_ids: HashMap<usize, TaggedHeapRef>,
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

    fn finalize(self) -> DumpTaggedHeap {
        DumpTaggedHeap {
            objects: self
                .objects
                .into_iter()
                .map(|obj| obj.unwrap_or(DumpHeapObject::Free))
                .collect(),
        }
    }
}

pub(crate) struct DumpEncoder {
    state: TaggedDumpState,
}

impl DumpEncoder {
    fn new() -> Self {
        Self {
            state: TaggedDumpState::new(),
        }
    }

    fn finalize(self) -> DumpTaggedHeap {
        self.state.finalize()
    }

    fn value_to_heap_ref(&mut self, v: &Value) -> TaggedHeapRef {
        debug_assert!(v.is_heap_object());
        let bits = v.bits();
        if let Some(id) = self.state.object_ids.get(&bits).copied() {
            return id;
        }

        let id = TaggedHeapRef {
            index: self.state.objects.len() as u32,
        };
        self.state.object_ids.insert(bits, id);
        self.state.objects.push(None);

        let dumped = dump_heap_object_from_value(self, *v);
        self.state.objects[id.index as usize] = Some(dumped);
        id
    }

    fn dump_float_id(&mut self, v: &Value) -> u32 {
        debug_assert!(v.is_float());
        let bits = v.bits();
        if let Some(id) = self.state.float_ids.get(&bits).copied() {
            return id;
        }
        let id = self.state.next_float_id;
        self.state.next_float_id += 1;
        self.state.float_ids.insert(bits, id);
        id
    }

    fn dump_value(&mut self, v: &Value) -> DumpValue {
        match v.kind() {
            ValueKind::Nil => DumpValue::Nil,
            ValueKind::T => DumpValue::True,
            ValueKind::Fixnum(n) => DumpValue::Int(n),
            ValueKind::Float => DumpValue::Float(v.xfloat(), self.dump_float_id(v)),
            ValueKind::Symbol(s) => DumpValue::Symbol(dump_sym_id(s)),
            ValueKind::String => DumpValue::Str(dump_heap_ref(self.value_to_heap_ref(v))),
            ValueKind::Cons => DumpValue::Cons(dump_heap_ref(self.value_to_heap_ref(v))),
            ValueKind::Veclike(VecLikeType::Vector) => {
                DumpValue::Vector(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Record) => {
                DumpValue::Record(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::HashTable) => {
                DumpValue::HashTable(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Lambda) => {
                DumpValue::Lambda(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Macro) => {
                DumpValue::Macro(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Subr) => {
                let s = v.as_subr_id().unwrap();
                DumpValue::Subr(dump_name_id(intern::symbol_name_id(s)))
            }
            ValueKind::Veclike(VecLikeType::ByteCode) => {
                DumpValue::ByteCode(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Marker) => {
                DumpValue::Marker(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Overlay) => {
                DumpValue::Overlay(dump_heap_ref(self.value_to_heap_ref(v)))
            }
            ValueKind::Veclike(VecLikeType::Buffer) => {
                DumpValue::Buffer(DumpBufferId(v.as_buffer_id().unwrap().0))
            }
            ValueKind::Veclike(VecLikeType::Window) => DumpValue::Window(v.as_window_id().unwrap()),
            ValueKind::Veclike(VecLikeType::Frame) => DumpValue::Frame(v.as_frame_id().unwrap()),
            ValueKind::Veclike(VecLikeType::Timer) => DumpValue::Timer(v.as_timer_id().unwrap()),
            ValueKind::Veclike(VecLikeType::Bignum) => {
                DumpValue::Bignum(v.as_bignum().unwrap().to_string())
            }
            ValueKind::Unbound => DumpValue::Unbound,
            ValueKind::Unknown => DumpValue::Nil,
        }
    }

    fn dump_opt_value(&mut self, v: &Option<Value>) -> Option<DumpValue> {
        v.as_ref().map(|value| self.dump_value(value))
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
    fn new(heap: &DumpTaggedHeap) -> Self {
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

pub(crate) struct LoadDecoder {
    state: TaggedLoadState,
}

impl LoadDecoder {
    pub(crate) fn new(heap: &DumpTaggedHeap) -> Self {
        Self {
            state: TaggedLoadState::new(heap),
        }
    }

    pub(crate) fn preload_tagged_heap(&mut self) -> Result<(), DumpError> {
        for index in 0..self.state.objects.len() {
            self.populate_tagged_object(TaggedHeapRef {
                index: index as u32,
            })?;
        }
        Ok(())
    }

    fn heap_ref_to_value(&mut self, id: TaggedHeapRef) -> Value {
        self.load_tagged_object(id)
    }

    fn load_float_value(&mut self, id: u32, value: f64) -> Value {
        *self
            .state
            .floats
            .entry(id)
            .or_insert_with(|| Value::make_float(value))
    }

    fn load_cached_buffer(&mut self, id: u64) -> Value {
        *self
            .state
            .buffers
            .entry(id)
            .or_insert_with(|| Value::make_buffer(BufferId(id)))
    }

    fn load_cached_window(&mut self, id: u64) -> Value {
        *self
            .state
            .windows
            .entry(id)
            .or_insert_with(|| Value::make_window(id))
    }

    fn load_cached_frame(&mut self, id: u64) -> Value {
        *self
            .state
            .frames
            .entry(id)
            .or_insert_with(|| Value::make_frame(id))
    }

    fn load_cached_timer(&mut self, id: u64) -> Value {
        *self
            .state
            .timers
            .entry(id)
            .or_insert_with(|| Value::make_timer(id))
    }

    fn allocate_tagged_placeholder(&mut self, id: TaggedHeapRef) -> Result<Value, DumpError> {
        if let Some(value) = self.state.values[id.index as usize] {
            return Ok(value);
        }
        let value = match &self.state.objects[id.index as usize] {
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
                data,
                size,
                size_byte,
                ..
            } => Value::heap_string(LispString::from_dump(data.clone(), *size, *size_byte)),
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
                gnu_bytecode_bytes: None,
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
            DumpHeapObject::Overlay(overlay) => {
                Value::make_overlay(crate::heap_types::OverlayData {
                    plist: Value::NIL,
                    buffer: overlay.buffer.map(|id| BufferId(id.0)),
                    start: overlay.start,
                    end: overlay.end,
                    front_advance: overlay.front_advance,
                    rear_advance: overlay.rear_advance,
                })
            }
            DumpHeapObject::Buffer(id) => self.load_cached_buffer(id.0),
            DumpHeapObject::Window(id) => self.load_cached_window(*id),
            DumpHeapObject::Frame(id) => self.load_cached_frame(*id),
            DumpHeapObject::Timer(id) => self.load_cached_timer(*id),
            DumpHeapObject::Subr { name, .. } => Value::subr_name_id(load_name_id(name)),
            DumpHeapObject::Free => Value::NIL,
        };
        self.state.values[id.index as usize] = Some(value);
        Ok(value)
    }

    fn populate_tagged_object(&mut self, id: TaggedHeapRef) -> Result<(), DumpError> {
        if self.state.populated[id.index as usize] {
            return Ok(());
        }

        let value = self.allocate_tagged_placeholder(id)?;
        self.state.populated[id.index as usize] = true;
        match self.state.objects[id.index as usize].clone() {
            DumpHeapObject::Cons { car, cdr } => {
                value.set_car(self.load_value(&car));
                value.set_cdr(self.load_value(&cdr));
            }
            DumpHeapObject::Vector(items) => {
                let _ = value
                    .replace_vector_data(items.iter().map(|item| self.load_value(item)).collect());
            }
            DumpHeapObject::HashTable(ht) => {
                let _ = value.with_hash_table_mut(|table| {
                    table.test = load_hash_table_test(&ht.test);
                    table.test_name = ht.test_name.map(|s| load_sym_id(&s));
                    table.size = ht.size;
                    table.weakness = ht.weakness.as_ref().map(load_hash_table_weakness);
                    table.rehash_size = ht.rehash_size;
                    table.rehash_threshold = ht.rehash_threshold;
                    table.data = ht
                        .entries
                        .iter()
                        .map(|(k, v)| (load_hash_key(self, k), self.load_value(v)))
                        .collect();
                    table.key_snapshots = ht
                        .key_snapshots
                        .iter()
                        .map(|(k, v)| (load_hash_key(self, k), self.load_value(v)))
                        .collect();
                    table.insertion_order = ht
                        .insertion_order
                        .iter()
                        .map(|key| load_hash_key(self, key))
                        .collect();
                });
            }
            DumpHeapObject::Str { text_props, .. } => {
                if !text_props.is_empty() {
                    let runs = text_props
                        .iter()
                        .map(|run| StringTextPropertyRun {
                            start: run.start,
                            end: run.end,
                            plist: self.load_value(&run.plist),
                        })
                        .collect();
                    set_string_text_properties_for_value(value, runs);
                }
            }
            DumpHeapObject::Float(_) => {}
            DumpHeapObject::Lambda(slots) | DumpHeapObject::Macro(slots) => {
                let _ = value.replace_closure_slots(
                    slots.iter().map(|slot| self.load_value(slot)).collect(),
                );
            }
            DumpHeapObject::ByteCode(bc) => {
                let _ = value
                    .with_bytecode_data_mut(|data| {
                        *data = load_bytecode(self, &bc)?;
                        Ok::<(), DumpError>(())
                    })
                    .transpose()?;
            }
            DumpHeapObject::Record(items) => {
                let _ = value
                    .replace_record_data(items.iter().map(|item| self.load_value(item)).collect());
            }
            DumpHeapObject::Marker(marker) => {
                let _ = value.with_marker_data_mut(|data| {
                    data.buffer = marker.buffer.map(|id| BufferId(id.0));
                    data.position = marker.position;
                    data.insertion_type = marker.insertion_type;
                    data.marker_id = marker.marker_id;
                });
            }
            DumpHeapObject::Overlay(overlay) => {
                let _ = value.with_overlay_data_mut(|data| {
                    data.plist = self.load_value(&overlay.plist);
                    data.buffer = overlay.buffer.map(|id| BufferId(id.0));
                    data.start = overlay.start;
                    data.end = overlay.end;
                    data.front_advance = overlay.front_advance;
                    data.rear_advance = overlay.rear_advance;
                });
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

    fn load_tagged_object(&mut self, id: TaggedHeapRef) -> Value {
        self.allocate_tagged_placeholder(id)
            .expect("pdump placeholder allocation should succeed");
        self.populate_tagged_object(id)
            .expect("pdump object population should succeed");
        self.state.values[id.index as usize].expect("pdump object should exist")
    }

    pub(crate) fn load_value(&mut self, v: &DumpValue) -> Value {
        match v {
            DumpValue::Nil => Value::NIL,
            DumpValue::True => Value::T,
            DumpValue::Int(n) => Value::fixnum(*n),
            DumpValue::Float(f, id) => self.load_float_value(*id, *f),
            DumpValue::Symbol(s) => Value::symbol(load_sym_id(s)),
            DumpValue::Str(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Cons(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Vector(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Record(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::HashTable(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Lambda(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Macro(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Subr(s) => Value::subr_name_id(load_name_id(s)),
            DumpValue::ByteCode(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Marker(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Overlay(id) => self.heap_ref_to_value(tagged_heap_ref(id)),
            DumpValue::Buffer(bid) => self.load_cached_buffer(bid.0),
            DumpValue::Window(w) => self.load_cached_window(*w),
            DumpValue::Frame(f) => self.load_cached_frame(*f),
            DumpValue::Timer(t) => self.load_cached_timer(*t),
            DumpValue::Bignum(text) => Value::make_integer_from_str_or_zero(text),
            DumpValue::Unbound => Value::UNBOUND,
        }
    }

    pub(crate) fn load_opt_value(&mut self, v: &Option<DumpValue>) -> Option<Value> {
        v.as_ref().map(|value| self.load_value(value))
    }
}

fn dump_heap_ref(id: TaggedHeapRef) -> DumpHeapRef {
    DumpHeapRef { index: id.index }
}

fn tagged_heap_ref(id: &DumpHeapRef) -> TaggedHeapRef {
    TaggedHeapRef { index: id.index }
}

// ===========================================================================
// Dump direction: Runtime → Dump
// ===========================================================================

// --- Primitives ---

pub(crate) fn dump_sym_id(id: SymId) -> DumpSymId {
    DumpSymId(id.0)
}

pub(crate) fn dump_name_id(id: NameId) -> DumpNameId {
    DumpNameId(id.0)
}

fn dump_lisp_string(string: &LispString) -> DumpLispString {
    DumpLispString {
        data: string.as_bytes().to_vec(),
        size: string.schars(),
        size_byte: if string.is_multibyte() {
            string.sbytes() as i64
        } else {
            -1
        },
    }
}

fn load_lisp_string(dump: &DumpLispString) -> LispString {
    LispString::from_dump(dump.data.clone(), dump.size, dump.size_byte)
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

pub(crate) fn dump_bytecode(
    encoder: &mut DumpEncoder,
    bc: &ByteCodeFunction,
) -> DumpByteCodeFunction {
    DumpByteCodeFunction {
        ops: bc.ops.iter().map(dump_op).collect(),
        constants: bc
            .constants
            .iter()
            .map(|value| encoder.dump_value(value))
            .collect(),
        max_stack: bc.max_stack,
        params: dump_lambda_params(&bc.params),
        lexical: bc.lexical,
        env: encoder.dump_opt_value(&bc.env),
        gnu_byte_offset_map: bc.gnu_byte_offset_map.as_ref().map(|map| {
            map.iter()
                .map(|(byte_off, instr_idx)| (*byte_off as u32, *instr_idx as u32))
                .collect()
        }),
        docstring: bc.docstring.clone(),
        doc_form: encoder.dump_opt_value(&bc.doc_form),
        interactive: encoder.dump_opt_value(&bc.interactive),
    }
}

// --- Hash tables ---

pub(crate) fn dump_hash_key(encoder: &mut DumpEncoder, k: &HashKey) -> DumpHashKey {
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
                let id = encoder.value_to_heap_ref(&value);
                DumpHashKey::HeapRef(id.index)
            } else {
                DumpHashKey::Ptr(*p as u64)
            }
        }
        HashKey::EqualCons(a, b) => DumpHashKey::EqualCons(
            Box::new(dump_hash_key(encoder, a)),
            Box::new(dump_hash_key(encoder, b)),
        ),
        HashKey::EqualVec(v) => {
            DumpHashKey::EqualVec(v.iter().map(|key| dump_hash_key(encoder, key)).collect())
        }
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

pub(crate) fn dump_hash_table(encoder: &mut DumpEncoder, ht: &LispHashTable) -> DumpLispHashTable {
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
            .map(|(k, v)| (dump_hash_key(encoder, k), encoder.dump_value(v)))
            .collect(),
        key_snapshots: ht
            .key_snapshots
            .iter()
            .map(|(k, v)| (dump_hash_key(encoder, k), encoder.dump_value(v)))
            .collect(),
        insertion_order: ht
            .insertion_order
            .iter()
            .map(|key| dump_hash_key(encoder, key))
            .collect(),
    }
}

// --- Heap objects ---

fn dump_closure_slots(encoder: &mut DumpEncoder, value: Value) -> Vec<DumpValue> {
    value
        .closure_slots()
        .map(|slots| slots.iter().map(|slot| encoder.dump_value(slot)).collect())
        .unwrap_or_default()
}

fn dump_heap_object_from_value(encoder: &mut DumpEncoder, value: Value) -> DumpHeapObject {
    match value.kind() {
        ValueKind::Cons => DumpHeapObject::Cons {
            car: encoder.dump_value(&value.cons_car()),
            cdr: encoder.dump_value(&value.cons_cdr()),
        },
        ValueKind::String => {
            let string = value.as_lisp_string().expect("string");
            DumpHeapObject::Str {
                data: string.as_bytes().to_vec(),
                size: string.schars(),
                size_byte: if string.is_multibyte() {
                    string.sbytes() as i64
                } else {
                    -1
                },
                text_props: get_string_text_properties_for_value(value)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|run| DumpStringTextPropertyRun {
                        start: run.start,
                        end: run.end,
                        plist: encoder.dump_value(&run.plist),
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
                .map(|item| encoder.dump_value(item))
                .collect(),
        ),
        ValueKind::Veclike(VecLikeType::HashTable) => DumpHeapObject::HashTable(dump_hash_table(
            encoder,
            value.as_hash_table().expect("hash-table"),
        )),
        ValueKind::Veclike(VecLikeType::Lambda) => {
            DumpHeapObject::Lambda(dump_closure_slots(encoder, value))
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            DumpHeapObject::Macro(dump_closure_slots(encoder, value))
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => DumpHeapObject::ByteCode(dump_bytecode(
            encoder,
            value.get_bytecode_data().expect("bytecode"),
        )),
        ValueKind::Veclike(VecLikeType::Record) => DumpHeapObject::Record(
            value
                .as_record_data()
                .expect("record")
                .iter()
                .map(|item| encoder.dump_value(item))
                .collect(),
        ),
        ValueKind::Veclike(VecLikeType::Overlay) => DumpHeapObject::Overlay(dump_overlay(
            encoder,
            value.as_overlay_data().expect("overlay"),
        )),
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
                name: dump_name_id(subr.name),
                min_args: subr.min_args,
                max_args: subr.max_args,
            }
        }
        _ => DumpHeapObject::Free,
    }
}

// --- Dump-wide symbol table ---

pub(crate) fn dump_symbol_table() -> DumpSymbolTable {
    let dumped = intern::dump_runtime_interner();
    DumpSymbolTable {
        names: dumped.names,
        symbols: dumped
            .symbol_names
            .into_iter()
            .zip(dumped.canonical)
            .map(|(name, canonical)| DumpSymbolEntry {
                name: DumpNameId(name),
                canonical,
            })
            .collect(),
    }
}

// --- Symbol / Obarray ---

fn dump_symbol_value(encoder: &mut DumpEncoder, sv: &SymbolValue) -> DumpSymbolValue {
    match sv {
        SymbolValue::Plain(v) => DumpSymbolValue::Plain(encoder.dump_opt_value(v)),
        SymbolValue::Alias(target) => DumpSymbolValue::Alias(dump_sym_id(*target)),
        SymbolValue::BufferLocal {
            default,
            local_if_set,
        } => DumpSymbolValue::BufferLocal {
            default: encoder.dump_opt_value(default),
            local_if_set: *local_if_set,
        },
        SymbolValue::Forwarded => DumpSymbolValue::Forwarded,
    }
}

pub(crate) fn dump_symbol_data(encoder: &mut DumpEncoder, sd: &SymbolData) -> DumpSymbolData {
    DumpSymbolData {
        name: None,
        value: None,
        symbol_value: Some(dump_symbol_value(encoder, &sd.value)),
        function: encoder.dump_opt_value(&sd.function),
        plist: sd
            .plist
            .iter()
            .map(|(k, v)| (dump_sym_id(*k), encoder.dump_value(v)))
            .collect(),
        special: sd.special,
        constant: sd.constant,
    }
}

pub(crate) fn dump_obarray(encoder: &mut DumpEncoder, ob: &Obarray) -> DumpObarray {
    DumpObarray {
        symbols: ob
            .iter_symbols()
            .map(|(id, sd)| (dump_sym_id(id), dump_symbol_data(encoder, sd)))
            .collect(),
        global_members: ob.global_member_ids().map(dump_sym_id).collect(),
        function_unbound: ob.function_unbound_ids().map(dump_sym_id).collect(),
        function_epoch: ob.function_epoch(),
    }
}

// --- OrderedSymMap ---

fn dump_runtime_binding_value(
    encoder: &mut DumpEncoder,
    value: &RuntimeBindingValue,
) -> DumpRuntimeBindingValue {
    match value {
        RuntimeBindingValue::Bound(value) => {
            DumpRuntimeBindingValue::Bound(encoder.dump_value(value))
        }
        RuntimeBindingValue::Void => DumpRuntimeBindingValue::Void,
    }
}

fn load_runtime_binding_value(
    decoder: &mut LoadDecoder,
    value: &DumpRuntimeBindingValue,
) -> RuntimeBindingValue {
    match value {
        DumpRuntimeBindingValue::Bound(value) => {
            RuntimeBindingValue::Bound(decoder.load_value(value))
        }
        DumpRuntimeBindingValue::Void => RuntimeBindingValue::Void,
    }
}

pub(crate) fn dump_ordered_sym_map(
    encoder: &mut DumpEncoder,
    m: &OrderedRuntimeBindingMap,
) -> DumpOrderedSymMap {
    DumpOrderedSymMap {
        entries: m
            .iter()
            .map(|(k, v)| (dump_sym_id(*k), dump_runtime_binding_value(encoder, v)))
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

fn dump_property_interval(
    encoder: &mut DumpEncoder,
    pi: &PropertyInterval,
) -> DumpPropertyInterval {
    DumpPropertyInterval {
        start: pi.start,
        end: pi.end,
        properties: pi
            .properties
            .iter()
            .map(|(k, v)| (encoder.dump_value(k), encoder.dump_value(v)))
            .collect(),
    }
}

fn dump_text_property_table(
    encoder: &mut DumpEncoder,
    tpt: &TextPropertyTable,
) -> DumpTextPropertyTable {
    DumpTextPropertyTable {
        intervals: tpt
            .dump_intervals()
            .into_iter()
            .map(|iv| dump_property_interval(encoder, &iv))
            .collect(),
    }
}

fn dump_overlay(encoder: &mut DumpEncoder, o: &Overlay) -> DumpOverlay {
    DumpOverlay {
        plist: encoder.dump_value(&o.plist),
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

fn dump_overlay_list(encoder: &mut DumpEncoder, ol: &OverlayList) -> DumpOverlayList {
    DumpOverlayList {
        overlays: ol
            .dump_overlays()
            .iter()
            .filter_map(|v| v.as_overlay_data())
            .map(|data| dump_overlay(encoder, data))
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

fn dump_buffer(encoder: &mut DumpEncoder, buf: &Buffer) -> DumpBuffer {
    let is_shared_text_owner = buf.base_buffer.is_none();
    DumpBuffer {
        id: DumpBufferId(buf.id.0),
        name: buf.name_runtime_string_owned(),
        base_buffer: buf.base_buffer.map(|id| DumpBufferId(id.0)),
        text: DumpGapBuffer {
            text: buf.text.dump_text(),
        },
        pt: buf.pt_byte,
        pt_char: Some(buf.pt),
        mark: buf.mark_byte,
        mark_char: buf.mark_char(),
        begv: buf.begv_byte,
        begv_char: Some(buf.begv),
        zv: buf.zv_byte,
        zv_char: Some(buf.zv),
        modified: buf.is_modified(),
        modified_tick: buf.modified_tick(),
        chars_modified_tick: buf.chars_modified_tick(),
        save_modified_tick: Some(buf.save_modified_tick()),
        autosave_modified_tick: Some(buf.autosave_modified_tick),
        last_window_start: Some(buf.last_window_start),
        read_only: buf.get_read_only(),
        multibyte: buf.get_multibyte(),
        file_name: buf.file_name_owned(),
        auto_save_file_name: buf.auto_save_file_name_owned(),
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
            .map(|(k, v)| (k, dump_runtime_binding_value(encoder, &v)))
            .collect(),
        local_binding_names: buf.ordered_buffer_local_names(),
        local_map: encoder.dump_value(&buf.local_map()),
        text_props: if is_shared_text_owner {
            dump_text_property_table(encoder, &buf.text.text_props_snapshot())
        } else {
            dump_text_property_table(encoder, &TextPropertyTable::new())
        },
        overlays: dump_overlay_list(encoder, &buf.overlays),
        syntax_table: dump_syntax_table(&buf.syntax_table),
        undo_list: None,
        // Phase 11.1: round-trip the BUFFER_OBJFWD slot table.
        // Previously blocked on the BLV GC trace bug (5699c3569);
        // with BLVs traced as roots, slot round-trip is safe for
        // the slot vector overall.
        slots: buf
            .slots
            .iter()
            .map(|slot| encoder.dump_value(slot))
            .collect(),
        // Phase 11: per-slot local-flag bitmap. Mirrors
        // `Buffer::local_flags` (Phase 10D bitset). Safe to
        // round-trip — it's a `u64`.
        local_flags: buf.local_flags,
        // Phase 11: per-buffer alist for SYMBOL_LOCALIZED variables.
        // Mirrors GNU `BVAR(buf, local_var_alist)`. The cons cells
        // already round-trip safely via the dump heap.
        local_var_alist: encoder.dump_value(&buf.local_var_alist),
    }
}

pub(crate) fn dump_buffer_manager(
    encoder: &mut DumpEncoder,
    bm: &BufferManager,
) -> DumpBufferManager {
    DumpBufferManager {
        buffers: bm
            .dump_buffers()
            .iter()
            .map(|(id, buf)| (DumpBufferId(id.0), dump_buffer(encoder, buf)))
            .collect(),
        current: bm.dump_current().map(|id| DumpBufferId(id.0)),
        next_id: bm.dump_next_id(),
        next_marker_id: bm.dump_next_marker_id(),
        // Mirror GNU's `buffer_defaults` C-static struct through the
        // dump. Without this, `setq-default` writes during loadup
        // (notably bindings.el's rich `mode-line-format`) are lost
        // on pdump-load, and `reset_buffer_local_variables` reverts
        // every conditional slot to its install-time seed.
        buffer_defaults: bm
            .buffer_defaults
            .iter()
            .map(|value| encoder.dump_value(value))
            .collect(),
    }
}

// --- Sub-managers ---

pub(crate) fn dump_autoload_manager(
    encoder: &mut DumpEncoder,
    am: &AutoloadManager,
) -> DumpAutoloadManager {
    DumpAutoloadManager {
        entries: am
            .dump_entries()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    DumpAutoloadEntry {
                        file: dump_lisp_string(&v.file),
                        docstring: v.docstring.as_ref().map(dump_lisp_string),
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
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter().map(|value| encoder.dump_value(value)).collect(),
                )
            })
            .collect(),
        loaded_files: am
            .dump_loaded_files()
            .iter()
            .map(dump_lisp_string)
            .collect(),
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

fn dump_mode_custom_type(encoder: &mut DumpEncoder, ct: &ModeCustomType) -> DumpModeCustomType {
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
                .map(|(s, v)| (s.clone(), encoder.dump_value(v)))
                .collect(),
        ),
        ModeCustomType::List(inner) => {
            DumpModeCustomType::List(Box::new(dump_mode_custom_type(encoder, inner)))
        }
        ModeCustomType::Alist(k, v) => DumpModeCustomType::Alist(
            Box::new(dump_mode_custom_type(encoder, k)),
            Box::new(dump_mode_custom_type(encoder, v)),
        ),
        ModeCustomType::Plist(k, v) => DumpModeCustomType::Plist(
            Box::new(dump_mode_custom_type(encoder, k)),
            Box::new(dump_mode_custom_type(encoder, v)),
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

pub(crate) fn dump_mode_registry(encoder: &mut DumpEncoder, mr: &ModeRegistry) -> DumpModeRegistry {
    DumpModeRegistry {
        major_modes: mr
            .dump_major_modes()
            .iter()
            .map(|(k, m)| {
                (
                    k.clone(),
                    DumpMajorMode {
                        pretty_name: m.pretty_name.clone(),
                        parent: m.parent.clone(),
                        mode_hook: m.mode_hook.clone(),
                        keymap_name: m.keymap_name.clone(),
                        syntax_table_name: m.syntax_table_name.clone(),
                        abbrev_table_name: m.abbrev_table_name.clone(),
                        font_lock: m.font_lock.as_ref().map(dump_font_lock_defaults),
                        body: encoder.dump_opt_value(&m.body),
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
                        lighter: m.lighter.clone(),
                        keymap_name: m.keymap_name.clone(),
                        global: m.global,
                        body: encoder.dump_opt_value(&m.body),
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
                        default_value: encoder.dump_value(&cv.default_value),
                        doc: cv.doc.clone(),
                        custom_type: dump_mode_custom_type(encoder, &cv.type_),
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

pub(crate) fn dump_coding_system_manager(
    encoder: &mut DumpEncoder,
    csm: &CodingSystemManager,
) -> DumpCodingSystemManager {
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
                            .map(|(k, v)| (k.clone(), encoder.dump_value(v)))
                            .collect(),
                        int_properties: v
                            .int_properties
                            .iter()
                            .map(|(k, v)| (*k, encoder.dump_value(v)))
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

pub(crate) fn dump_charset_registry(encoder: &mut DumpEncoder) -> DumpCharsetRegistry {
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
                    .map(|(key, value)| (key, encoder.dump_value(&value)))
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
        foreground: f.foreground.map(|c| dump_color(&c)),
        background: f.background.map(|c| dump_color(&c)),
        family: f.family_runtime_string_owned(),
        foundry: f.foundry_runtime_string_owned(),
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
        stipple: f.stipple.and_then(|value| value.as_runtime_string_owned()),
        extend: f.extend,
        inherit: f
            .inherit
            .iter()
            .filter_map(|value| value.as_symbol_name().map(str::to_string))
            .collect(),
        overstrike: f.overstrike,
        doc: f.doc.and_then(|value| value.as_runtime_string_owned()),
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
        killed: r.killed.iter().map(dump_lisp_string).collect(),
    }
}

pub(crate) fn dump_kmacro(encoder: &mut DumpEncoder, km: &KmacroManager) -> DumpKmacroManager {
    DumpKmacroManager {
        // Live recording/playback state is keyboard-runtime owned and is not
        // persisted in fresh dumps. Keep the fields for backward-compatible
        // decoding of older pdumps only.
        current_macro: Vec::new(),
        last_macro: None,
        macro_ring: km
            .macro_ring
            .iter()
            .map(|m| m.iter().map(|value| encoder.dump_value(value)).collect())
            .collect(),
        counter: km.counter,
        counter_format: km.counter_format.clone(),
    }
}

pub(crate) fn dump_register_manager(
    encoder: &mut DumpEncoder,
    rm: &RegisterManager,
) -> DumpRegisterManager {
    DumpRegisterManager {
        registers: rm
            .dump_registers()
            .iter()
            .map(|(c, r)| {
                (
                    *c,
                    match r {
                        RegisterContent::Text(s) => DumpRegisterContent::Text {
                            data: s.as_bytes().to_vec(),
                            size: s.schars(),
                            size_byte: if s.is_multibyte() {
                                s.sbytes() as i64
                            } else {
                                -1
                            },
                        },
                        RegisterContent::Number(n) => DumpRegisterContent::Number(*n),
                        RegisterContent::Marker(v) => {
                            DumpRegisterContent::Marker(encoder.dump_value(v))
                        }
                        RegisterContent::Rectangle(lines) => DumpRegisterContent::Rectangle(
                            lines.iter().map(dump_lisp_string).collect(),
                        ),
                        RegisterContent::FrameConfig(v) => {
                            DumpRegisterContent::FrameConfig(encoder.dump_value(v))
                        }
                        RegisterContent::File(s) => DumpRegisterContent::File(dump_lisp_string(s)),
                        RegisterContent::KbdMacro(keys) => DumpRegisterContent::KbdMacro(
                            keys.iter().map(|value| encoder.dump_value(value)).collect(),
                        ),
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
                        name: dump_lisp_string(&b.name),
                        filename: b
                            .filename
                            .as_ref()
                            .map(crate::emacs_core::builtins::runtime_string_from_lisp_string),
                        position: b.position,
                        front_context: b
                            .front_context
                            .as_ref()
                            .map(crate::emacs_core::builtins::runtime_string_from_lisp_string),
                        rear_context: b
                            .rear_context
                            .as_ref()
                            .map(crate::emacs_core::builtins::runtime_string_from_lisp_string),
                        annotation: b
                            .annotation
                            .as_ref()
                            .map(crate::emacs_core::builtins::runtime_string_from_lisp_string),
                        handler: b
                            .handler
                            .as_ref()
                            .map(crate::emacs_core::builtins::runtime_string_from_lisp_string),
                    },
                )
            })
            .collect(),
        recent: bm.dump_recent().iter().map(dump_lisp_string).collect(),
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
                        name: dump_lisp_string(&t.name),
                        abbrevs: t
                            .abbrevs
                            .iter()
                            .map(|(k, a)| {
                                (
                                    k.clone(),
                                    DumpAbbrev {
                                        expansion: dump_lisp_string(&a.expansion),
                                        hook: a.hook.as_ref().map(dump_lisp_string),
                                        count: a.count,
                                        system: a.system,
                                    },
                                )
                            })
                            .collect(),
                        parent: t.parent.as_ref().map(dump_lisp_string),
                        case_fixed: t.case_fixed,
                        enable_quoting: t.enable_quoting,
                    },
                )
            })
            .collect(),
        global_table_name: dump_lisp_string(am.dump_global_table_name()),
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

pub(crate) fn dump_watcher_list(
    encoder: &mut DumpEncoder,
    wl: &VariableWatcherList,
) -> DumpVariableWatcherList {
    DumpVariableWatcherList {
        watchers: wl
            .dump_watchers()
            .iter()
            .map(|(k, watchers)| {
                (
                    dump_sym_id(*k),
                    watchers
                        .iter()
                        .map(|w| encoder.dump_value(&w.callback))
                        .collect(),
                )
            })
            .collect(),
    }
}

pub(crate) fn dump_string_text_prop_run(
    encoder: &mut DumpEncoder,
    r: &StringTextPropertyRun,
) -> DumpStringTextPropertyRun {
    DumpStringTextPropertyRun {
        start: r.start,
        end: r.end,
        plist: encoder.dump_value(&r.plist),
    }
}

/// Convert a TextPropertyTable to a list of DumpPropertyInterval entries (for string props).
/// Does NOT allocate heap objects — serializes property values directly.
fn dump_string_text_property_table(
    encoder: &mut DumpEncoder,
    table: &TextPropertyTable,
) -> Vec<DumpPropertyInterval> {
    let mut intervals = Vec::new();
    for iv in table.dump_intervals() {
        if iv.properties.is_empty() {
            continue;
        }
        let properties: Vec<(DumpValue, DumpValue)> = iv
            .properties
            .iter()
            .map(|(key, val)| (encoder.dump_value(key), encoder.dump_value(val)))
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
    let mut encoder = DumpEncoder::new();

    let dump = DumpContextState {
        symbol_table: dump_symbol_table(),
        tagged_heap: DumpTaggedHeap {
            objects: Vec::new(),
        },
        obarray: dump_obarray(&mut encoder, &eval.obarray),
        dynamic: Vec::new(),
        lexenv: encoder.dump_value(&eval.lexenv),
        features: eval.features.iter().copied().map(dump_sym_id).collect(),
        require_stack: eval
            .require_stack
            .iter()
            .copied()
            .map(dump_sym_id)
            .collect(),
        loads_in_progress: eval
            .loads_in_progress
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        buffers: dump_buffer_manager(&mut encoder, &eval.buffers),
        autoloads: dump_autoload_manager(&mut encoder, &eval.autoloads),
        custom: dump_custom_manager(&eval.custom),
        modes: dump_mode_registry(&mut encoder, &eval.modes),
        coding_systems: dump_coding_system_manager(&mut encoder, &eval.coding_systems),
        charset_registry: dump_charset_registry(&mut encoder),
        fontset_registry: dump_fontset_registry(),
        face_table: dump_face_table(&eval.face_table),
        abbrevs: dump_abbrev_manager(&eval.abbrevs),
        interactive: dump_interactive_registry(&eval.interactive),
        rectangle: dump_rectangle(&eval.rectangle),
        standard_syntax_table: encoder.dump_value(&eval.standard_syntax_table),
        standard_category_table: encoder.dump_value(&eval.standard_category_table),
        current_local_map: encoder.dump_value(&eval.current_local_map),
        kmacro: dump_kmacro(&mut encoder, &eval.kmacro),
        registers: dump_register_manager(&mut encoder, &eval.registers),
        bookmarks: dump_bookmark_manager(&eval.bookmarks),
        watchers: dump_watcher_list(&mut encoder, &eval.watchers),
    };

    let tagged_heap = encoder.finalize();

    DumpContextState {
        tagged_heap,
        ..dump
    }
}

// ===========================================================================
// Load direction: Dump → Runtime
// ===========================================================================

// --- Primitives ---

pub(crate) fn load_sym_id(id: &DumpSymId) -> SymId {
    // Dump symbol slots are local to the serialized interner ordering. They
    // must be translated back into the current process interner before any
    // runtime value or object can safely refer to them.
    PDUMP_LOAD_SYM_REMAP.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|remap| remap.get(id.0 as usize))
            .copied()
            .unwrap_or_else(|| panic!("pdump symbol slot {} should have a runtime remap", id.0))
    })
}

pub(crate) fn load_name_id(id: &DumpNameId) -> NameId {
    PDUMP_LOAD_NAME_REMAP.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|remap| remap.get(id.0 as usize))
            .copied()
            .unwrap_or_else(|| panic!("pdump name slot {} should have a runtime remap", id.0))
    })
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

pub(crate) fn load_bytecode(
    decoder: &mut LoadDecoder,
    bc: &DumpByteCodeFunction,
) -> Result<ByteCodeFunction, DumpError> {
    Ok(ByteCodeFunction {
        ops: bc.ops.iter().map(load_op).collect::<Result<Vec<_>, _>>()?,
        constants: bc
            .constants
            .iter()
            .map(|value| decoder.load_value(value))
            .collect(),
        max_stack: bc.max_stack,
        params: load_lambda_params(&bc.params),
        lexical: bc.lexical,
        env: decoder.load_opt_value(&bc.env),
        gnu_byte_offset_map: bc.gnu_byte_offset_map.as_ref().map(|pairs| {
            pairs
                .iter()
                .map(|(byte_off, instr_idx)| (*byte_off as usize, *instr_idx as usize))
                .collect()
        }),
        gnu_bytecode_bytes: None,
        docstring: bc.docstring.clone(),
        doc_form: decoder.load_opt_value(&bc.doc_form),
        interactive: decoder.load_opt_value(&bc.interactive),
    })
}

// --- Hash tables ---

pub(crate) fn load_hash_key(decoder: &mut LoadDecoder, k: &DumpHashKey) -> HashKey {
    match k {
        DumpHashKey::Nil => HashKey::Nil,
        DumpHashKey::True => HashKey::True,
        DumpHashKey::Int(n) => HashKey::Int(*n),
        DumpHashKey::Float(bits) => HashKey::Float(*bits),
        DumpHashKey::FloatEq(bits, id) => HashKey::FloatEq(*bits, *id),
        DumpHashKey::Symbol(s) => HashKey::Symbol(load_sym_id(s)),
        DumpHashKey::Keyword(s) => HashKey::Keyword(load_sym_id(s)),
        DumpHashKey::Str(id) => HashKey::Ptr(decoder.heap_ref_to_value(tagged_heap_ref(id)).bits()),
        DumpHashKey::Char(c) => HashKey::Char(*c),
        DumpHashKey::Window(w) => HashKey::Window(*w),
        DumpHashKey::Frame(f) => HashKey::Frame(*f),
        DumpHashKey::Ptr(p) => HashKey::Ptr(*p as usize),
        DumpHashKey::HeapRef(a) => HashKey::Ptr(
            decoder
                .heap_ref_to_value(TaggedHeapRef { index: *a })
                .bits(),
        ),
        DumpHashKey::EqualCons(a, b) => HashKey::EqualCons(
            Box::new(load_hash_key(decoder, a)),
            Box::new(load_hash_key(decoder, b)),
        ),
        DumpHashKey::EqualVec(v) => {
            HashKey::EqualVec(v.iter().map(|item| load_hash_key(decoder, item)).collect())
        }
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

pub(crate) fn load_hash_table(decoder: &mut LoadDecoder, ht: &DumpLispHashTable) -> LispHashTable {
    let data: HashMap<HashKey, Value> = ht
        .entries
        .iter()
        .map(|(k, v)| (load_hash_key(decoder, k), decoder.load_value(v)))
        .collect();
    let key_snapshots: HashMap<HashKey, Value> = ht
        .key_snapshots
        .iter()
        .map(|(k, v)| (load_hash_key(decoder, k), decoder.load_value(v)))
        .collect();
    let insertion_order: Vec<HashKey> = ht
        .insertion_order
        .iter()
        .map(|key| load_hash_key(decoder, key))
        .collect();

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

// --- Dump-wide symbol table ---

pub(crate) fn load_symbol_table(table: &DumpSymbolTable) -> Result<(), DumpError> {
    let symbol_names: Vec<u32> = table.symbols.iter().map(|entry| entry.name.0).collect();
    let canonical: Vec<bool> = table.symbols.iter().map(|entry| entry.canonical).collect();
    let remap = intern::restore_runtime_interner(&table.names, &symbol_names, Some(&canonical))
        .map_err(DumpError::DeserializationError)?;
    let intern::RestoredDumpSymbolTable { names, symbols } = remap;
    PDUMP_LOAD_NAME_REMAP.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_none(),
            "pdump name remap should not already be initialized"
        );
        *slot = Some(names);
    });
    PDUMP_LOAD_SYM_REMAP.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_none(),
            "pdump symbol remap should not already be initialized"
        );
        *slot = Some(symbols);
    });
    Ok(())
}

pub(crate) fn finish_load_interner() {
    PDUMP_LOAD_NAME_REMAP.with(|slot| {
        slot.borrow_mut().take();
    });
    PDUMP_LOAD_SYM_REMAP.with(|slot| {
        slot.borrow_mut().take();
    });
}

// --- Symbol / Obarray ---

fn load_symbol_value_enum(decoder: &mut LoadDecoder, dsv: &DumpSymbolValue) -> SymbolValue {
    match dsv {
        DumpSymbolValue::Plain(v) => SymbolValue::Plain(decoder.load_opt_value(v)),
        DumpSymbolValue::Alias(target) => SymbolValue::Alias(load_sym_id(target)),
        DumpSymbolValue::BufferLocal {
            default,
            local_if_set,
        } => SymbolValue::BufferLocal {
            default: decoder.load_opt_value(default),
            local_if_set: *local_if_set,
        },
        DumpSymbolValue::Forwarded => SymbolValue::Forwarded,
    }
}

pub(crate) fn load_symbol_data(
    decoder: &mut LoadDecoder,
    sym_id: SymId,
    sd: &DumpSymbolData,
) -> SymbolData {
    // Prefer the new `symbol_value` field; fall back to legacy `value` field
    // for backward compatibility with older pdump files.
    let value = if let Some(ref sv) = sd.symbol_value {
        load_symbol_value_enum(decoder, sv)
    } else {
        SymbolValue::Plain(decoder.load_opt_value(&sd.value))
    };
    let mut symbol = SymbolData::new(sym_id);
    // Mirror the legacy `value` cell into the new redirect-shape fields
    // (Phase 1 of the symbol-redirect refactor — both representations
    // are kept in sync until Phase 4-10 removes the legacy enum).
    use crate::emacs_core::symbol::{SymbolRedirect, SymbolVal};
    match &value {
        SymbolValue::Plain(v) => {
            symbol.flags.set_redirect(SymbolRedirect::Plainval);
            symbol.val = SymbolVal {
                plain: v.unwrap_or(crate::emacs_core::value::Value::NIL),
            };
        }
        SymbolValue::Alias(target) => {
            symbol.set_alias_target(*target);
        }
        SymbolValue::BufferLocal { default, .. } => {
            // Phase 1: BufferLocal still rides on Plainval until the BLV
            // dispatch lands in Phase 4. The default lives in `val.plain`.
            symbol.flags.set_redirect(SymbolRedirect::Plainval);
            symbol.val = SymbolVal {
                plain: default.unwrap_or(crate::emacs_core::value::Value::NIL),
            };
        }
        SymbolValue::Forwarded => {
            // Phase 1: forwarded symbols are not yet round-tripped through
            // the redirect (Phase 8 wires it up). Leave the new fields at
            // their default Plainval / NIL.
        }
    }
    symbol.value = value;
    symbol.function = decoder.load_opt_value(&sd.function);
    symbol.plist = sd
        .plist
        .iter()
        .map(|(k, v)| (load_sym_id(k), decoder.load_value(v)))
        .collect();
    symbol.special = sd.special;
    symbol.constant = sd.constant;
    symbol
}

pub(crate) fn load_obarray(
    decoder: &mut LoadDecoder,
    dob: &DumpObarray,
) -> Result<Obarray, DumpError> {
    let mut seen_symbol_ids = FxHashSet::default();
    let mut symbols = Vec::with_capacity(dob.symbols.len());
    for (id, sd) in &dob.symbols {
        let sym_id = load_sym_id(id);
        if !seen_symbol_ids.insert(sym_id) {
            return Err(DumpError::DeserializationError(format!(
                "pdump obarray is inconsistent: duplicate symbol slot {}",
                sym_id.0
            )));
        }
        symbols.push((sym_id, load_symbol_data(decoder, sym_id, sd)));
    }

    let load_member_set = |label: &str, ids: &[DumpSymId]| -> Result<Vec<SymId>, DumpError> {
        let mut seen = FxHashSet::default();
        let mut loaded = Vec::with_capacity(ids.len());
        for id in ids {
            let sym_id = load_sym_id(id);
            if !seen.insert(sym_id) {
                return Err(DumpError::DeserializationError(format!(
                    "pdump obarray is inconsistent: duplicate {label} entry for symbol slot {}",
                    sym_id.0
                )));
            }
            if !seen_symbol_ids.contains(&sym_id) {
                return Err(DumpError::DeserializationError(format!(
                    "pdump obarray is inconsistent: {label} entry references missing symbol slot {}",
                    sym_id.0
                )));
            }
            loaded.push(sym_id);
        }
        Ok(loaded)
    };

    let global_members = load_member_set("global_members", &dob.global_members)?;
    let function_unbound = load_member_set("function_unbound", &dob.function_unbound)?;

    Ok(Obarray::from_dump(
        symbols,
        global_members,
        function_unbound,
        dob.function_epoch,
    ))
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

fn load_property_interval(
    decoder: &mut LoadDecoder,
    pi: &DumpPropertyInterval,
) -> PropertyInterval {
    let properties: std::collections::HashMap<
        crate::emacs_core::value::Value,
        crate::emacs_core::value::Value,
    > = pi
        .properties
        .iter()
        .map(|(k, v)| (decoder.load_value(k), decoder.load_value(v)))
        .collect();
    let key_order: Vec<crate::emacs_core::value::Value> = pi
        .properties
        .iter()
        .map(|(k, _)| decoder.load_value(k))
        .collect();
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

fn load_buffer(decoder: &mut LoadDecoder, db: &DumpBuffer) -> Buffer {
    let text = BufferText::from_dump(db.text.text.clone(), db.multibyte);
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
    // Phase 10F: the legacy `BufferLocals` struct is gone.
    // Reconstruct per-buffer state from the dump's properties list
    // directly into the new storage model:
    //
    //   * `buffer-undo-list` → `SharedUndoState` (the one
    //     always-present non-slot non-alist binding).
    //   * Slot-backed names (BUFFER_OBJFWD) → already restored via
    //     the `slots: ...` round-trip below; skip here.
    //   * Everything else → `local_var_alist`, walked in the
    //     original `local_binding_names` order so the dumped
    //     ordering is preserved.
    let loaded_keymap = decoder.load_value(&db.local_map);
    let mut loaded_properties: std::collections::HashMap<String, RuntimeBindingValue> = db
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), load_runtime_binding_value(decoder, v)))
        .collect();
    let mut loaded_undo_list = Value::NIL;
    if let Some(RuntimeBindingValue::Bound(value)) = loaded_properties.remove("buffer-undo-list") {
        loaded_undo_list = value;
    }
    // Reconstruct the alist in the ordered sequence the dump recorded,
    // falling back to sorted remainder for any properties missing from
    // the ordered list. Skip entries that map to BUFFER_OBJFWD slots
    // (they live in the slot table).
    let mut loaded_local_var_alist = Value::NIL;
    let prepend_alist_entry = |alist: &mut Value, name: &str, binding: RuntimeBindingValue| {
        if crate::buffer::buffer::lookup_buffer_slot(name).is_some() {
            return;
        }
        let RuntimeBindingValue::Bound(value) = binding else {
            return;
        };
        let key = Value::from_sym_id(crate::emacs_core::intern::intern(name));
        let cell = Value::cons(key, value);
        *alist = Value::cons(cell, *alist);
    };
    // Walk ordered names first (preserves relative ordering).
    // Because we prepend, iterate in reverse to restore the
    // original head-first order.
    for name in db.local_binding_names.iter().rev() {
        if name == "buffer-undo-list" {
            continue;
        }
        if let Some(binding) = loaded_properties.remove(name) {
            prepend_alist_entry(&mut loaded_local_var_alist, name, binding);
        }
    }
    // Any remaining unordered properties (older dumps that didn't
    // carry `local_binding_names`) get appended in sorted order.
    let mut remaining: Vec<_> = loaded_properties.into_iter().collect();
    remaining.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, binding) in remaining.into_iter().rev() {
        if name == "buffer-undo-list" {
            continue;
        }
        prepend_alist_entry(&mut loaded_local_var_alist, &name, binding);
    }
    let undo_list = loaded_undo_list;

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
            .map(|interval| load_property_interval(decoder, interval))
            .collect(),
    );
    text.text_props_replace(text_props);

    text.set_modification_state(db.modified_tick, db.chars_modified_tick, save_modified_tick);

    Buffer {
        id: BufferId(db.id.0),
        name: Value::string(db.name.clone()),
        base_buffer: db.base_buffer.map(|id| BufferId(id.0)),
        text,
        pt: pt_char,
        pt_byte: db.pt,
        mark: mark_char,
        mark_byte: db.mark,
        begv: begv_char,
        begv_byte: db.begv,
        zv: zv_char,
        zv_byte: db.zv,
        autosave_modified_tick,
        last_window_start,
        last_selected_window: None,
        inhibit_buffer_hooks: false,
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
        // Phase 10F: per-buffer alist for SYMBOL_LOCALIZED variables.
        // Prefer the dump's `local_var_alist` field when present
        // (new format). Fall back to the alist we rebuilt from the
        // legacy `properties` table for older dumps that didn't
        // carry the alist directly.
        local_var_alist: {
            let dumped = decoder.load_value(&db.local_var_alist);
            if dumped.is_nil() && !loaded_local_var_alist.is_nil() {
                loaded_local_var_alist
            } else {
                dumped
            }
        },
        // Phase 10F: `BVAR(buf, keymap)` — the buffer's local
        // keymap, previously stored inside `BufferLocals::local_map`.
        keymap: loaded_keymap,
        // Phase 11.1: round-trip BUFFER_OBJFWD slots through pdump.
        // Previously blocked on the BLV GC trace bug (5699c3569);
        // with BLVs now traced as roots, slot Values stay live
        // through GCs in `apply_runtime_startup_state` and the
        // round-trip is safe. Falls back to per-slot defaults from
        // `BUFFER_SLOT_INFO` for any slot the dump didn't carry
        // (older format compatibility, or sentinel buffers without
        // a populated slot vector).
        slots: {
            let mut s =
                [crate::emacs_core::value::Value::NIL; crate::buffer::buffer::BUFFER_SLOT_COUNT];
            for info in crate::buffer::buffer::BUFFER_SLOT_INFO {
                s[info.offset] = info.default.to_value();
            }
            for (idx, dumped) in db.slots.iter().enumerate() {
                if idx >= crate::buffer::buffer::BUFFER_SLOT_COUNT {
                    break;
                }
                s[idx] = decoder.load_value(dumped);
            }
            // Legacy header field overrides (older dump compat).
            if let Some(ref fname) = db.file_name {
                s[crate::buffer::buffer::BUFFER_SLOT_FILE_NAME] =
                    crate::emacs_core::value::Value::string(fname);
            }
            if let Some(ref asname) = db.auto_save_file_name {
                s[crate::buffer::buffer::BUFFER_SLOT_AUTO_SAVE_FILE_NAME] =
                    crate::emacs_core::value::Value::string(asname);
            }
            if db.read_only {
                s[crate::buffer::buffer::BUFFER_SLOT_READ_ONLY] =
                    crate::emacs_core::value::Value::T;
            }
            if db.multibyte {
                s[crate::buffer::buffer::BUFFER_SLOT_ENABLE_MULTIBYTE_CHARACTERS] =
                    crate::emacs_core::value::Value::T;
            }
            s
        },
        // Phase 11: per-buffer local-flags bitmap round-trip.
        local_flags: db.local_flags,
        overlays: OverlayList::from_dump(
            db.overlays
                .overlays
                .iter()
                .map(|d| {
                    Value::make_overlay(crate::heap_types::OverlayData {
                        plist: decoder.load_value(&d.plist),
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

pub(crate) fn load_buffer_manager(
    decoder: &mut LoadDecoder,
    dbm: &DumpBufferManager,
) -> BufferManager {
    let buffers: HashMap<BufferId, Buffer> = dbm
        .buffers
        .iter()
        .map(|(id, buf)| (BufferId(id.0), load_buffer(decoder, buf)))
        .collect();
    // New in the current dump format: `buffer_defaults` ride through
    // pdump so runtime `setq-default` writes survive. Older dumps
    // (no `buffer_defaults` field) deserialize as an empty Vec via
    // `#[serde(default)]`, and `BufferManager::from_dump` then falls
    // back to the install-time seeds from `BUFFER_SLOT_INFO`.
    let defaults_values: Vec<crate::emacs_core::value::Value> = dbm
        .buffer_defaults
        .iter()
        .map(|value| decoder.load_value(value))
        .collect();
    let dumped_defaults = if defaults_values.is_empty() {
        None
    } else {
        Some(defaults_values.as_slice())
    };
    BufferManager::from_dump(
        buffers,
        dbm.current.map(|id| BufferId(id.0)),
        dbm.next_id,
        dbm.next_marker_id,
        dumped_defaults,
    )
}

// --- Sub-managers ---

pub(crate) fn load_autoload_manager(
    decoder: &mut LoadDecoder,
    dam: &DumpAutoloadManager,
) -> AutoloadManager {
    let entries: HashMap<String, AutoloadEntry> = dam
        .entries
        .iter()
        .map(|(k, e)| {
            (
                k.clone(),
                AutoloadEntry {
                    file: load_lisp_string(&e.file),
                    docstring: e.docstring.as_ref().map(load_lisp_string),
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
        .map(|(k, v)| {
            (
                k.clone(),
                v.iter().map(|value| decoder.load_value(value)).collect(),
            )
        })
        .collect();
    AutoloadManager::from_dump(
        entries,
        after_load,
        dam.loaded_files.iter().map(load_lisp_string).collect(),
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

fn load_mode_custom_type(decoder: &mut LoadDecoder, ct: &DumpModeCustomType) -> ModeCustomType {
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
                .map(|(s, v)| (s.clone(), decoder.load_value(v)))
                .collect(),
        ),
        DumpModeCustomType::List(inner) => {
            ModeCustomType::List(Box::new(load_mode_custom_type(decoder, inner)))
        }
        DumpModeCustomType::Alist(k, v) => ModeCustomType::Alist(
            Box::new(load_mode_custom_type(decoder, k)),
            Box::new(load_mode_custom_type(decoder, v)),
        ),
        DumpModeCustomType::Plist(k, v) => ModeCustomType::Plist(
            Box::new(load_mode_custom_type(decoder, k)),
            Box::new(load_mode_custom_type(decoder, v)),
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

pub(crate) fn load_mode_registry(
    decoder: &mut LoadDecoder,
    dmr: &DumpModeRegistry,
) -> ModeRegistry {
    let major_modes: HashMap<String, MajorMode> = dmr
        .major_modes
        .iter()
        .map(|(k, m)| {
            (
                k.clone(),
                MajorMode {
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
                    body: decoder.load_opt_value(&m.body),
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
                    lighter: m.lighter.clone(),
                    keymap_name: m.keymap_name.clone(),
                    global: m.global,
                    body: decoder.load_opt_value(&m.body),
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
                    default_value: decoder.load_value(&cv.default_value),
                    doc: cv.doc.clone(),
                    type_: load_mode_custom_type(decoder, &cv.custom_type),
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

pub(crate) fn load_coding_system_manager(
    decoder: &mut LoadDecoder,
    dcsm: &DumpCodingSystemManager,
) -> CodingSystemManager {
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
                        .map(|(k, v)| (k.clone(), decoder.load_value(v)))
                        .collect(),
                    int_properties: v
                        .int_properties
                        .iter()
                        .map(|(k, v)| (*k, decoder.load_value(v)))
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

pub(crate) fn load_charset_registry(decoder: &mut LoadDecoder, dcr: &DumpCharsetRegistry) {
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
                    .map(|(key, value)| (key.clone(), decoder.load_value(value)))
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
        foreground: df.foreground.map(|c| load_color(&c)),
        background: df.background.map(|c| load_color(&c)),
        family: df.family.as_ref().map(Value::string),
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
        stipple: df.stipple.as_ref().map(Value::string),
        extend: df.extend,
        inherit: df
            .inherit
            .iter()
            .map(|name| Value::symbol(name.as_str()))
            .collect(),
        overstrike: df.overstrike,
        doc: df.doc.as_ref().map(Value::string),
        overline_color: None,
        strike_through_color: None,
        distant_foreground: None,
        foundry: df.foundry.as_ref().map(Value::string),
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
        killed: dr.killed.iter().map(load_lisp_string).collect(),
    }
}

pub(crate) fn load_kmacro(decoder: &mut LoadDecoder, dkm: &DumpKmacroManager) -> KmacroManager {
    KmacroManager {
        macro_ring: dkm
            .macro_ring
            .iter()
            .map(|m| m.iter().map(|value| decoder.load_value(value)).collect())
            .collect(),
        counter: dkm.counter,
        counter_format: dkm.counter_format.clone(),
    }
}

pub(crate) fn load_register_manager(
    decoder: &mut LoadDecoder,
    drm: &DumpRegisterManager,
) -> RegisterManager {
    let registers: HashMap<char, RegisterContent> = drm
        .registers
        .iter()
        .map(|(c, r)| {
            (
                *c,
                match r {
                    DumpRegisterContent::Text {
                        data,
                        size,
                        size_byte,
                    } => RegisterContent::Text(LispString::from_dump(
                        data.clone(),
                        *size,
                        *size_byte,
                    )),
                    DumpRegisterContent::Number(n) => RegisterContent::Number(*n),
                    DumpRegisterContent::Marker(v) => {
                        RegisterContent::Marker(decoder.load_value(v))
                    }
                    DumpRegisterContent::Rectangle(lines) => {
                        RegisterContent::Rectangle(lines.iter().map(load_lisp_string).collect())
                    }
                    DumpRegisterContent::FrameConfig(v) => {
                        RegisterContent::FrameConfig(decoder.load_value(v))
                    }
                    DumpRegisterContent::File(s) => RegisterContent::File(load_lisp_string(s)),
                    DumpRegisterContent::KbdMacro(keys) => RegisterContent::KbdMacro(
                        keys.iter().map(|value| decoder.load_value(value)).collect(),
                    ),
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
                    name: load_lisp_string(&b.name),
                    filename: b.filename.as_deref().map(|s| {
                        crate::emacs_core::builtins::runtime_string_to_lisp_string(s, true)
                    }),
                    position: b.position,
                    front_context: b.front_context.as_deref().map(|s| {
                        crate::emacs_core::builtins::runtime_string_to_lisp_string(s, true)
                    }),
                    rear_context: b.rear_context.as_deref().map(|s| {
                        crate::emacs_core::builtins::runtime_string_to_lisp_string(s, true)
                    }),
                    annotation: b.annotation.as_deref().map(|s| {
                        crate::emacs_core::builtins::runtime_string_to_lisp_string(s, true)
                    }),
                    handler: b.handler.as_deref().map(|s| {
                        crate::emacs_core::builtins::runtime_string_to_lisp_string(s, true)
                    }),
                },
            )
        })
        .collect();
    BookmarkManager::from_dump(bookmarks, dbm.recent.iter().map(load_lisp_string).collect())
}

pub(crate) fn load_abbrev_manager(dam: &DumpAbbrevManager) -> AbbrevManager {
    let tables: HashMap<String, AbbrevTable> = dam
        .tables
        .iter()
        .map(|(k, t)| {
            (
                k.clone(),
                AbbrevTable {
                    name: load_lisp_string(&t.name),
                    abbrevs: t
                        .abbrevs
                        .iter()
                        .map(|(k, a)| {
                            (
                                k.clone(),
                                Abbrev {
                                    expansion: load_lisp_string(&a.expansion),
                                    hook: a.hook.as_ref().map(load_lisp_string),
                                    count: a.count,
                                    system: a.system,
                                },
                            )
                        })
                        .collect(),
                    parent: t.parent.as_ref().map(load_lisp_string),
                    case_fixed: t.case_fixed,
                    enable_quoting: t.enable_quoting,
                },
            )
        })
        .collect();
    AbbrevManager::from_dump(
        tables,
        load_lisp_string(&dam.global_table_name),
        dam.abbrev_mode,
    )
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

pub(crate) fn load_watcher_list(
    decoder: &mut LoadDecoder,
    dwl: &DumpVariableWatcherList,
) -> VariableWatcherList {
    let watchers: HashMap<SymId, Vec<VariableWatcher>> = dwl
        .watchers
        .iter()
        .map(|(k, callbacks)| {
            (
                load_sym_id(k),
                callbacks
                    .iter()
                    .map(|v| VariableWatcher {
                        callback: decoder.load_value(v),
                    })
                    .collect(),
            )
        })
        .collect();
    VariableWatcherList::from_dump(watchers)
}

pub(crate) fn load_string_text_prop_run(
    decoder: &mut LoadDecoder,
    r: &DumpStringTextPropertyRun,
) -> StringTextPropertyRun {
    StringTextPropertyRun {
        start: r.start,
        end: r.end,
        plist: decoder.load_value(&r.plist),
    }
}

/// Convert a list of DumpPropertyInterval entries back to a TextPropertyTable.
pub(crate) fn load_text_property_table(
    decoder: &mut LoadDecoder,
    intervals: &[DumpPropertyInterval],
) -> TextPropertyTable {
    let mut table = TextPropertyTable::new();
    for iv in intervals {
        for (name, dump_val) in &iv.properties {
            let val = decoder.load_value(dump_val);
            table.put_property(iv.start, iv.end, decoder.load_value(name), val);
        }
    }
    table
}
