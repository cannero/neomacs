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
use crate::emacs_core::symbol::{LispSymbol, Obarray, SymbolTrappedWrite};
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
            ValueKind::Subr(s) => {
                DumpValue::Subr(dump_name_id(intern::symbol_name_id(s)))
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
            ValueKind::Veclike(VecLikeType::SymbolWithPos) => {
                // SymbolWithPos cannot be portably serialized in a pdump yet.
                // Signal an error so callers know this case is unimplemented.
                panic!("pdump: symbol-with-pos is not yet supported in portable dumps")
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
                bytepos: 0,
                charpos: 0,
                next_marker: std::ptr::null_mut(),
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
            DumpHeapObject::Subr { name, .. } => {
                let name_id = load_name_id(name);
                if let Some(sym_id) = intern::canonical_symbol_for_name(name_id) {
                    Value::subr_from_sym_id(sym_id)
                } else {
                    let n = intern::resolve_name(name_id);
                    Value::subr_from_sym_id(intern::intern(n))
                }
            }
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
            DumpValue::Subr(s) => {
                let name_id = load_name_id(s);
                // Convert NameId -> SymId for immediate subr encoding
                if let Some(sym_id) = intern::canonical_symbol_for_name(name_id) {
                    Value::subr_from_sym_id(sym_id)
                } else {
                    // Fallback: intern the name to get a SymId
                    let name = intern::resolve_name(name_id);
                    Value::subr_from_sym_id(intern::intern(name))
                }
            }
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

pub(super) fn load_lisp_string(dump: &DumpLispString) -> LispString {
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
        Op::SaveWindowExcursion => DumpOp::SaveWindowExcursion,
        Op::MakeClosure(n) => DumpOp::MakeClosure(n),
        Op::CallBuiltin(a, b) => DumpOp::CallBuiltin(a, b),
        Op::CallBuiltinSym(sym, b) => DumpOp::CallBuiltinSym(dump_sym_id(sym), b),
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
        docstring: bc.docstring.as_ref().map(dump_lisp_string),
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

pub(crate) fn dump_symbol_data(encoder: &mut DumpEncoder, sd: &LispSymbol) -> DumpSymbolData {
    // Phase I (pdump v21): encode redirect + flags directly.
    use crate::emacs_core::symbol::{SymbolInterned, SymbolRedirect};
    let redirect = sd.flags.redirect();
    let val = match redirect {
        SymbolRedirect::Plainval => {
            let v = unsafe { sd.val.plain };
            // Preserve the UNBOUND sentinel — DumpValue::Unbound maps back to
            // Value::UNBOUND on load, which is the correct "unbound" state.
            DumpSymbolVal::Plain(encoder.dump_value(&v))
        }
        SymbolRedirect::Varalias => {
            let target = unsafe { sd.val.alias };
            DumpSymbolVal::Alias(dump_sym_id(target))
        }
        SymbolRedirect::Localized => {
            // Read the BLV to get the global default and local_if_set flag.
            // The BLV is heap-allocated and valid while sd is alive.
            let (default, local_if_set) = unsafe {
                let blv = &*sd.val.blv;
                let default_val = blv.defcell.cons_cdr();
                (encoder.dump_value(&default_val), blv.local_if_set)
            };
            DumpSymbolVal::Localized {
                default,
                local_if_set,
            }
        }
        SymbolRedirect::Forwarded => {
            // BUFFER_OBJFWD forwarders are re-installed from BUFFER_SLOT_INFO
            // at load time (see reconstruct_evaluator).  Nothing to encode.
            DumpSymbolVal::Forwarded
        }
    };
    DumpSymbolData {
        redirect: redirect as u8,
        trapped_write: sd.flags.trapped_write() as u8,
        interned: sd.flags.interned() as u8,
        declared_special: sd.flags.declared_special(),
        val,
        function: encoder.dump_value(&sd.function),
        plist: encoder.dump_value(&sd.plist),
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

// dump_undo_record and dump_undo_list removed — undo state is now a
// buffer-local Lisp Value serialized through the properties map.

fn dump_buffer(encoder: &mut DumpEncoder, buf: &Buffer) -> DumpBuffer {
    let is_shared_text_owner = buf.base_buffer.is_none();
    DumpBuffer {
        id: DumpBufferId(buf.id.0),
        name_lisp: buf.name_value().as_lisp_string().map(dump_lisp_string),
        name: None,
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
        file_name_lisp: buf.file_name_lisp_string().map(dump_lisp_string),
        file_name: None,
        auto_save_file_name_lisp: buf.auto_save_file_name_lisp_string().map(dump_lisp_string),
        auto_save_file_name: None,
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
        properties_syms: buf
            .ordered_buffer_local_bindings()
            .into_iter()
            .map(|(sym_id, value)| {
                (
                    dump_sym_id(sym_id),
                    dump_runtime_binding_value(encoder, &value),
                )
            })
            .collect(),
        properties: Vec::new(),
        local_binding_syms: buf
            .ordered_buffer_local_names()
            .into_iter()
            .map(dump_sym_id)
            .collect(),
        local_binding_names: Vec::new(),
        local_map: encoder.dump_value(&buf.local_map()),
        text_props: if is_shared_text_owner {
            dump_text_property_table(encoder, &buf.text.text_props_snapshot())
        } else {
            dump_text_property_table(encoder, &TextPropertyTable::new())
        },
        overlays: dump_overlay_list(encoder, &buf.overlays),
        // Syntax table lives in `buf.slots[BUFFER_SLOT_SYNTAX_TABLE]`
        // (serialized via the slots Vec below) — matches GNU where
        // `buffer->syntax_table` is a single Lisp_Object slot.
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
        entries_syms: am
            .dump_entries()
            .iter()
            .map(|(k, v)| {
                (
                    dump_sym_id(*k),
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
        entries: Vec::new(),
        after_load_lisp: am
            .dump_after_load()
            .iter()
            .map(|(k, v)| {
                (
                    dump_lisp_string(k.as_lisp_string()),
                    v.iter().map(|value| encoder.dump_value(value)).collect(),
                )
            })
            .collect(),
        after_load: Vec::new(),
        loaded_files: am
            .dump_loaded_files()
            .iter()
            .map(dump_lisp_string)
            .collect(),
        obsolete_functions_syms: am
            .dump_obsolete_functions()
            .iter()
            .map(|(name, (new_name, when))| {
                (
                    dump_sym_id(*name),
                    (dump_lisp_string(new_name), dump_lisp_string(when)),
                )
            })
            .collect(),
        obsolete_functions: Vec::new(),
        obsolete_variables_syms: am
            .dump_obsolete_variables()
            .iter()
            .map(|(name, (new_name, when))| {
                (
                    dump_sym_id(*name),
                    (dump_lisp_string(new_name), dump_lisp_string(when)),
                )
            })
            .collect(),
        obsolete_variables: Vec::new(),
    }
}

pub(crate) fn dump_custom_manager(_cm: &CustomManager) -> DumpCustomManager {
    // Phase D: auto_buffer_local mirror removed. Emit empty vecs so that
    // existing pdump readers that check the field for backward compat
    // still see a valid (empty) payload.
    DumpCustomManager {
        auto_buffer_local_syms: Vec::new(),
        auto_buffer_local: Vec::new(),
    }
}

fn dump_font_lock_keyword(kw: &FontLockKeyword) -> DumpFontLockKeyword {
    DumpFontLockKeyword {
        pattern_lisp: Some(dump_lisp_string(&kw.pattern)),
        pattern: None,
        face_sym: Some(dump_sym_id(kw.face)),
        face: None,
        group: kw.group,
        override_: kw.override_,
        laxmatch: kw.laxmatch,
    }
}

fn dump_font_lock_defaults(fld: &FontLockDefaults) -> DumpFontLockDefaults {
    DumpFontLockDefaults {
        keywords: fld.keywords.iter().map(dump_font_lock_keyword).collect(),
        case_fold: fld.case_fold,
        syntax_table_lisp: fld.syntax_table.as_ref().map(dump_lisp_string),
        syntax_table: None,
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
                    dump_sym_id(*k),
                    DumpMajorMode {
                        pretty_name: dump_lisp_string(&m.pretty_name),
                        parent: encoder.dump_opt_value(&m.parent),
                        mode_hook: encoder.dump_value(&m.mode_hook),
                        keymap_name: encoder.dump_opt_value(&m.keymap_name),
                        syntax_table_name: encoder.dump_opt_value(&m.syntax_table_name),
                        abbrev_table_name: encoder.dump_opt_value(&m.abbrev_table_name),
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
                    dump_sym_id(*k),
                    DumpMinorMode {
                        lighter: m.lighter.as_ref().map(dump_lisp_string),
                        keymap_name: encoder.dump_opt_value(&m.keymap_name),
                        global: m.global,
                        body: encoder.dump_opt_value(&m.body),
                    },
                )
            })
            .collect(),
        buffer_major_modes: mr
            .dump_buffer_major_modes()
            .iter()
            .map(|(k, v)| (*k, encoder.dump_value(v)))
            .collect(),
        buffer_minor_modes: mr
            .dump_buffer_minor_modes()
            .iter()
            .map(|(k, v)| {
                (
                    *k,
                    v.iter().map(|value| encoder.dump_value(value)).collect(),
                )
            })
            .collect(),
        global_minor_modes: mr
            .dump_global_minor_modes()
            .iter()
            .map(|value| encoder.dump_value(value))
            .collect(),
        auto_mode_alist: Vec::new(),
        auto_mode_alist_lisp: mr
            .dump_auto_mode_alist()
            .iter()
            .map(|(pattern, value)| (dump_lisp_string(pattern), encoder.dump_value(value)))
            .collect(),
        custom_variables: mr
            .dump_custom_variables()
            .iter()
            .map(|(k, cv)| {
                (
                    dump_sym_id(*k),
                    DumpModeCustomVariable {
                        default_value: encoder.dump_value(&cv.default_value),
                        doc: cv.doc.as_ref().map(dump_lisp_string),
                        custom_type: dump_mode_custom_type(encoder, &cv.type_),
                        group: encoder.dump_opt_value(&cv.group),
                        set_function: encoder.dump_opt_value(&cv.set_function),
                        get_function: encoder.dump_opt_value(&cv.get_function),
                        tag: cv.tag.as_ref().map(dump_lisp_string),
                    },
                )
            })
            .collect(),
        custom_groups: mr
            .dump_custom_groups()
            .iter()
            .map(|(k, g)| {
                (
                    dump_sym_id(*k),
                    DumpModeCustomGroup {
                        doc: g.doc.as_ref().map(dump_lisp_string),
                        parent: encoder.dump_opt_value(&g.parent),
                        members: g
                            .members
                            .iter()
                            .map(|value| encoder.dump_value(value))
                            .collect(),
                    },
                )
            })
            .collect(),
        fundamental_mode: encoder.dump_value(&mr.dump_fundamental_mode()),
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
        systems_syms: csm
            .systems
            .iter()
            .map(|(k, v)| {
                (
                    dump_sym_id(*k),
                    DumpCodingSystemInfo {
                        name_sym: Some(dump_sym_id(v.name)),
                        name: None,
                        coding_type_sym: Some(dump_sym_id(v.coding_type)),
                        coding_type: None,
                        mnemonic: v.mnemonic,
                        eol_type: dump_eol_type(&v.eol_type),
                        ascii_compatible_p: v.ascii_compatible_p,
                        charset_list_syms: v
                            .charset_list
                            .iter()
                            .map(|id| dump_sym_id(*id))
                            .collect(),
                        charset_list: Vec::new(),
                        post_read_conversion_sym: v.post_read_conversion.map(dump_sym_id),
                        post_read_conversion: None,
                        pre_write_conversion_sym: v.pre_write_conversion.map(dump_sym_id),
                        pre_write_conversion: None,
                        default_char: v.default_char,
                        for_unibyte: v.for_unibyte,
                        properties_syms: v
                            .properties
                            .iter()
                            .map(|(k, v)| (dump_sym_id(*k), encoder.dump_value(v)))
                            .collect(),
                        properties: Vec::new(),
                        int_properties: v
                            .int_properties
                            .iter()
                            .map(|(k, v)| (*k, encoder.dump_value(v)))
                            .collect(),
                    },
                )
            })
            .collect(),
        systems: Vec::new(),
        aliases_syms: csm
            .aliases
            .iter()
            .map(|(k, v)| (dump_sym_id(*k), dump_sym_id(*v)))
            .collect(),
        aliases: Vec::new(),
        priority_syms: csm.priority.iter().map(|id| dump_sym_id(*id)).collect(),
        priority: Vec::new(),
        keyboard_coding_sym: Some(dump_sym_id(csm.dump_keyboard_coding_sym())),
        keyboard_coding: None,
        terminal_coding_sym: Some(dump_sym_id(csm.dump_terminal_coding_sym())),
        terminal_coding: None,
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
                name_sym: Some(dump_sym_id(info.name)),
                name: None,
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
                unify_map: encoder.dump_value(&info.unify_map),
                method: match info.method {
                    CharsetMethodSnapshot::Offset(offset) => DumpCharsetMethod::Offset(offset),
                    CharsetMethodSnapshot::Map(map_name) => DumpCharsetMethod::Map(map_name),
                    CharsetMethodSnapshot::Subset(subset) => {
                        DumpCharsetMethod::Subset(DumpCharsetSubsetSpec {
                            parent_sym: Some(dump_sym_id(subset.parent)),
                            parent: None,
                            parent_min_code: subset.parent_min_code,
                            parent_max_code: subset.parent_max_code,
                            offset: subset.offset,
                        })
                    }
                    CharsetMethodSnapshot::Superset(members) => DumpCharsetMethod::SupersetSyms(
                        members
                            .into_iter()
                            .map(|(name, offset)| (dump_sym_id(name), offset))
                            .collect(),
                    ),
                },
                plist_syms: info
                    .plist
                    .into_iter()
                    .map(|(key, value)| (dump_sym_id(key), encoder.dump_value(&value)))
                    .collect(),
                plist: Vec::new(),
            })
            .collect(),
        priority_syms: snapshot.priority.into_iter().map(dump_sym_id).collect(),
        priority: Vec::new(),
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
        FontRepertory::Charset(name) => DumpFontRepertory::CharsetSym(dump_sym_id(name)),
        FontRepertory::CharTableRanges(ranges) => DumpFontRepertory::CharTableRanges(ranges),
    }
}

fn dump_stored_font_spec(spec: StoredFontSpec) -> DumpStoredFontSpec {
    DumpStoredFontSpec {
        family_sym: spec.family.map(dump_sym_id),
        family: None,
        registry_sym: spec.registry.map(dump_sym_id),
        registry: None,
        lang_sym: spec.lang.map(dump_sym_id),
        lang: None,
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
        ordered_names_lisp: snapshot
            .ordered_names
            .iter()
            .map(dump_lisp_string)
            .collect(),
        alias_to_name_lisp: snapshot
            .alias_to_name
            .iter()
            .map(|(alias, name)| (dump_lisp_string(alias), dump_lisp_string(name)))
            .collect(),
        fontsets_lisp: snapshot
            .fontsets
            .iter()
            .map(|(name, data)| {
                (
                    dump_lisp_string(name),
                    DumpFontsetData {
                        ranges: data
                            .ranges
                            .iter()
                            .map(|range| DumpFontsetRangeEntry {
                                from: range.from,
                                to: range.to,
                                entries: range
                                    .entries
                                    .iter()
                                    .cloned()
                                    .map(dump_font_spec_entry)
                                    .collect(),
                            })
                            .collect(),
                        fallback: data.fallback.as_ref().map(|entries| {
                            entries.iter().cloned().map(dump_font_spec_entry).collect()
                        }),
                    },
                )
            })
            .collect(),
        ordered_names: Vec::new(),
        alias_to_name: Vec::new(),
        fontsets: Vec::new(),
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

fn dump_face(encoder: &mut DumpEncoder, f: &Face) -> DumpFace {
    DumpFace {
        foreground: f.foreground.map(|c| dump_color(&c)),
        background: f.background.map(|c| dump_color(&c)),
        family_value: f.family.as_ref().map(|value| encoder.dump_value(value)),
        family: None,
        foundry_value: f.foundry.as_ref().map(|value| encoder.dump_value(value)),
        foundry: None,
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
        stipple_value: f.stipple.as_ref().map(|value| encoder.dump_value(value)),
        stipple: None,
        extend: f.extend,
        // Legacy dump schema: flatten the face_ref into a symbol list.
        // A symbol becomes a one-element list; a list of symbols is
        // preserved; plists and nested refs are dropped (a later schema
        // revision should store the raw face_ref with full fidelity).
        inherit_syms: match f.inherit {
            None => Vec::new(),
            Some(v) => {
                if let Some(id) = v.as_symbol_id() {
                    vec![dump_sym_id(id)]
                } else {
                    crate::emacs_core::value::list_to_vec(&v)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|entry| entry.as_symbol_id().map(dump_sym_id))
                                .collect()
                        })
                        .unwrap_or_default()
                }
            }
        },
        inherit: Vec::new(),
        overstrike: f.overstrike,
        doc_value: f.doc.as_ref().map(|value| encoder.dump_value(value)),
        doc: None,
    }
}

pub(crate) fn dump_face_table(encoder: &mut DumpEncoder, ft: &FaceTable) -> DumpFaceTable {
    DumpFaceTable {
        face_ids: ft
            .dump_faces_by_sym_id()
            .into_iter()
            .map(|(id, f)| (dump_sym_id(id), dump_face(encoder, &f)))
            .collect(),
        faces: Vec::new(),
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
        counter_format_lisp: Some(dump_lisp_string(&km.counter_format)),
        counter_format: None,
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
        bookmarks_lisp: bm
            .dump_bookmarks()
            .iter()
            .map(|(k, b)| {
                (
                    dump_lisp_string(k.as_lisp_string()),
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
        bookmarks: Vec::new(),
        recent: bm.dump_recent().iter().map(dump_lisp_string).collect(),
    }
}

pub(crate) fn dump_abbrev_manager(am: &AbbrevManager) -> DumpAbbrevManager {
    DumpAbbrevManager {
        tables_syms: am
            .dump_tables()
            .iter()
            .map(|(sym, t)| {
                (
                    dump_sym_id(*sym),
                    DumpAbbrevTable {
                        name: dump_lisp_string(&t.name),
                        abbrevs: t
                            .abbrevs
                            .iter()
                            .map(|(k, a)| {
                                (
                                    dump_lisp_string(k),
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
        tables: Vec::new(),
        global_table_sym: Some(dump_sym_id(am.dump_global_table_sym())),
        global_table_name: dump_lisp_string(&am.global_table_name()),
        abbrev_mode: am.dump_abbrev_mode(),
    }
}

pub(crate) fn dump_interactive_registry(
    encoder: &mut DumpEncoder,
    ir: &InteractiveRegistry,
) -> DumpInteractiveRegistry {
    DumpInteractiveRegistry {
        specs: ir
            .dump_specs()
            .iter()
            .map(|(k, s)| {
                (
                    dump_sym_id(*k),
                    DumpInteractiveSpec {
                        spec: encoder.dump_value(&s.spec),
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
            .map(dump_lisp_string)
            .collect(),
        buffers: dump_buffer_manager(&mut encoder, &eval.buffers),
        autoloads: dump_autoload_manager(&mut encoder, &eval.autoloads),
        custom: dump_custom_manager(&eval.custom),
        modes: dump_mode_registry(&mut encoder, &eval.modes),
        coding_systems: dump_coding_system_manager(&mut encoder, &eval.coding_systems),
        charset_registry: dump_charset_registry(&mut encoder),
        fontset_registry: dump_fontset_registry(),
        face_table: dump_face_table(&mut encoder, &eval.face_table),
        abbrevs: dump_abbrev_manager(&eval.abbrevs),
        interactive: dump_interactive_registry(&mut encoder, &eval.interactive),
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
        DumpOp::SaveWindowExcursion => Op::SaveWindowExcursion,
        DumpOp::MakeClosure(n) => Op::MakeClosure(n),
        DumpOp::CallBuiltin(a, b) => Op::CallBuiltin(a, b),
        DumpOp::CallBuiltinSym(sym, b) => Op::CallBuiltinSym(load_sym_id(&sym), b),
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
        docstring: bc.docstring.as_ref().map(load_lisp_string),
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

pub(crate) fn load_symbol_data(
    decoder: &mut LoadDecoder,
    sym_id: SymId,
    sd: &DumpSymbolData,
) -> LispSymbol {
    use crate::emacs_core::symbol::{SymbolInterned, SymbolRedirect, SymbolVal};
    let mut symbol = LispSymbol::new(sym_id);

    // Restore flag fields.  The `redirect` field is also encoded in `val`'s
    // variant, but we set it here explicitly for clarity.
    let trapped_write: SymbolTrappedWrite = unsafe { std::mem::transmute(sd.trapped_write & 0b11) };
    let interned: SymbolInterned = unsafe { std::mem::transmute(sd.interned & 0b11) };
    symbol.flags.set_trapped_write(trapped_write);
    symbol.flags.set_interned(interned);
    symbol.flags.set_declared_special(sd.declared_special);

    match &sd.val {
        DumpSymbolVal::Plain(v) => {
            symbol.flags.set_redirect(SymbolRedirect::Plainval);
            symbol.val = SymbolVal {
                plain: decoder.load_value(v),
            };
        }
        DumpSymbolVal::Alias(target) => {
            symbol.set_alias_target(load_sym_id(target));
        }
        DumpSymbolVal::Localized { default, .. } => {
            // BLV reconstruction requires the Obarray to be live so that
            // make_symbol_localized can allocate and track the BLV pointer.
            // We cannot do it here (we don't have &mut Obarray).  Instead
            // we store the default in val.plain temporarily; load_obarray
            // performs a second pass after Obarray::from_dump to call
            // make_symbol_localized on every Localized symbol and fix the
            // redirect + BLV pointer.
            symbol.flags.set_redirect(SymbolRedirect::Plainval);
            symbol.val = SymbolVal {
                plain: decoder.load_value(default),
            };
        }
        DumpSymbolVal::Forwarded => {
            // BUFFER_OBJFWD forwarders are re-installed from BUFFER_SLOT_INFO
            // in reconstruct_evaluator after the obarray is built.  Leave the
            // redirect at Plainval / UNBOUND for now; reconstruct_evaluator
            // will call install_buffer_objfwd which flips it to Forwarded.
            symbol.flags.set_redirect(SymbolRedirect::Plainval);
            symbol.val = SymbolVal {
                plain: crate::emacs_core::value::Value::UNBOUND,
            };
        }
    }

    symbol.function = decoder.load_value(&sd.function);
    symbol.plist = decoder.load_value(&sd.plist);
    symbol
}

pub(crate) fn load_obarray(
    decoder: &mut LoadDecoder,
    dob: &DumpObarray,
) -> Result<Obarray, DumpError> {
    let mut seen_symbol_ids = FxHashSet::default();
    // Collect (sym_id, dump_data) for a second pass over Localized symbols.
    let mut localized_entries: Vec<(SymId, &DumpSymbolData)> = Vec::new();
    let mut symbols = Vec::with_capacity(dob.symbols.len());
    for (id, sd) in &dob.symbols {
        let sym_id = load_sym_id(id);
        if !seen_symbol_ids.insert(sym_id) {
            return Err(DumpError::DeserializationError(format!(
                "pdump obarray is inconsistent: duplicate symbol slot {}",
                sym_id.0
            )));
        }
        if matches!(sd.val, DumpSymbolVal::Localized { .. }) {
            localized_entries.push((sym_id, sd));
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

    let mut obarray = Obarray::from_dump(
        symbols,
        global_members,
        function_unbound,
        dob.function_epoch,
    );

    // Second pass: reconstruct BLVs for LOCALIZED symbols.
    //
    // load_symbol_data temporarily stored the global default in val.plain
    // (with redirect=Plainval) because BLV allocation requires a live
    // &mut Obarray.  Now that the obarray is built we can call
    // make_symbol_localized to allocate and install the real BLV, then
    // optionally set local_if_set.
    for (sym_id, sd) in &localized_entries {
        if let DumpSymbolVal::Localized {
            default,
            local_if_set,
        } = &sd.val
        {
            let default_val = decoder.load_value(default);
            obarray.make_symbol_localized(*sym_id, default_val);
            if *local_if_set {
                obarray.set_blv_local_if_set(*sym_id, true);
            }
            // Restore non-redirect flags from the dump — make_symbol_localized
            // only sets the redirect bit, leaving trapped_write / interned /
            // declared_special as defaults.  Re-apply them from the dump.
            use crate::emacs_core::symbol::SymbolInterned;
            if let Some(sym) = obarray.get_mut_by_id(*sym_id) {
                let trapped_write: SymbolTrappedWrite =
                    unsafe { std::mem::transmute(sd.trapped_write & 0b11) };
                let interned: SymbolInterned =
                    unsafe { std::mem::transmute(sd.interned & 0b11) };
                sym.flags.set_trapped_write(trapped_write);
                sym.flags.set_interned(interned);
                sym.flags.set_declared_special(sd.declared_special);
            }
        }
    }

    Ok(obarray)
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
        // Resolve the backing MarkerObj allocated during
        // `preload_tagged_heap`. If pdump dumped a MarkerEntry whose
        // marker_id has no corresponding live MarkerObj (possible for
        // older dumps or GC-dropped markers), we still need to register
        // something for Vec-based readers, so allocate a fresh scratch
        // MarkerObj. The scratch is not reachable as a Lisp value but is
        // tracked in `TaggedHeap::marker_ptrs` and will be swept if it
        // stays unreferenced.
        let marker_ptr = with_tagged_heap(|heap| heap.find_marker_by_id(marker.id))
            .unwrap_or_else(|| {
                let scratch =
                    crate::emacs_core::value::Value::make_marker(crate::heap_types::MarkerData {
                        buffer: Some(marker.buffer_id),
                        position: None,
                        insertion_type: marker.insertion_type
                            == crate::buffer::InsertionType::After,
                        marker_id: Some(marker.id),
                        bytepos: 0,
                        charpos: 0,
                        next_marker: std::ptr::null_mut(),
                    });
                scratch
                    .as_veclike_ptr()
                    .expect("freshly allocated marker should have a veclike ptr")
                    as *mut crate::tagged::header::MarkerObj
            });
        // The MarkerObj may already be on a chain from a prior load in
        // the same process (e.g. reload after a bootstrap). Unlink
        // defensively from this buffer's chain before splicing.
        text.chain_unlink(marker_ptr);
        text.register_marker(
            marker_ptr,
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
    //     original `local_binding_syms` order so the dumped
    //     ordering is preserved.
    let loaded_keymap = decoder.load_value(&db.local_map);
    let mut loaded_properties: std::collections::HashMap<SymId, RuntimeBindingValue> =
        if db.properties_syms.is_empty() {
            db.properties
                .iter()
                .map(|(name, value)| {
                    (
                        intern::intern(name),
                        load_runtime_binding_value(decoder, value),
                    )
                })
                .collect()
        } else {
            db.properties_syms
                .iter()
                .map(|(sym_id, value)| {
                    (
                        load_sym_id(sym_id),
                        load_runtime_binding_value(decoder, value),
                    )
                })
                .collect()
        };
    let mut loaded_undo_list = Value::NIL;
    if let Some(RuntimeBindingValue::Bound(value)) =
        loaded_properties.remove(&intern::intern("buffer-undo-list"))
    {
        loaded_undo_list = value;
    }
    // Reconstruct the alist in the ordered sequence the dump recorded,
    // falling back to sorted remainder for any properties missing from
    // the ordered list. Skip entries that map to BUFFER_OBJFWD slots
    // (they live in the slot table).
    let mut loaded_local_var_alist = Value::NIL;
    let prepend_alist_entry = |alist: &mut Value, sym_id: SymId, binding: RuntimeBindingValue| {
        if crate::buffer::buffer::lookup_buffer_slot(intern::resolve_sym(sym_id)).is_some() {
            return;
        }
        let RuntimeBindingValue::Bound(value) = binding else {
            return;
        };
        let key = Value::from_sym_id(sym_id);
        let cell = Value::cons(key, value);
        *alist = Value::cons(cell, *alist);
    };
    // Walk ordered names first (preserves relative ordering).
    // Because we prepend, iterate in reverse to restore the
    // original head-first order.
    let ordered_local_bindings: Vec<SymId> = if db.local_binding_syms.is_empty() {
        db.local_binding_names
            .iter()
            .map(|name| intern::intern(name))
            .collect()
    } else {
        db.local_binding_syms.iter().map(load_sym_id).collect()
    };
    for sym_id in ordered_local_bindings.into_iter().rev() {
        if sym_id == intern::intern("buffer-undo-list") {
            continue;
        }
        if let Some(binding) = loaded_properties.remove(&sym_id) {
            prepend_alist_entry(&mut loaded_local_var_alist, sym_id, binding);
        }
    }
    // Any remaining unordered properties (older dumps that didn't
    // carry `local_binding_syms`) get appended in sorted order.
    let mut remaining: Vec<_> = loaded_properties.into_iter().collect();
    remaining.sort_by(|left, right| intern::resolve_sym(left.0).cmp(intern::resolve_sym(right.0)));
    for (sym_id, binding) in remaining.into_iter().rev() {
        if sym_id == intern::intern("buffer-undo-list") {
            continue;
        }
        prepend_alist_entry(&mut loaded_local_var_alist, sym_id, binding);
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
        name: if let Some(ref name) = db.name_lisp {
            Value::heap_string(load_lisp_string(name))
        } else {
            Value::string(db.name.clone().unwrap_or_default())
        },
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
                // Resolve each state marker's backing MarkerObj pointer from
                // the tagged heap (allocated via `preload_tagged_heap`). If
                // the dumped `state_*_marker` id has no live MarkerObj, fall
                // back to a fresh scratch allocation so the chain stays valid
                // for the dual-write Vec path.
                let resolve = |mid: u64| -> *mut crate::tagged::header::MarkerObj {
                    with_tagged_heap(|heap| heap.find_marker_by_id(mid))
                        .unwrap_or_else(|| {
                            let scratch = crate::emacs_core::value::Value::make_marker(
                                crate::heap_types::MarkerData {
                                    buffer: Some(BufferId(db.id.0)),
                                    position: None,
                                    insertion_type: false,
                                    marker_id: Some(mid),
                                    bytepos: 0,
                                    charpos: 0,
                                    next_marker: std::ptr::null_mut(),
                                },
                            );
                            scratch
                                .as_veclike_ptr()
                                .expect("freshly allocated marker should have a veclike ptr")
                                as *mut crate::tagged::header::MarkerObj
                        })
                };
                Some(crate::buffer::buffer::BufferStateMarkers {
                    pt_marker,
                    begv_marker,
                    zv_marker,
                    pt_marker_ptr: resolve(pt_marker),
                    begv_marker_ptr: resolve(begv_marker),
                    zv_marker_ptr: resolve(zv_marker),
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
            if let Some(ref fname) = db.file_name_lisp {
                s[crate::buffer::buffer::BUFFER_SLOT_FILE_NAME] =
                    crate::emacs_core::value::Value::heap_string(load_lisp_string(fname));
            } else if let Some(ref fname) = db.file_name {
                s[crate::buffer::buffer::BUFFER_SLOT_FILE_NAME] =
                    crate::emacs_core::value::Value::string(fname);
            }
            if let Some(ref asname) = db.auto_save_file_name_lisp {
                s[crate::buffer::buffer::BUFFER_SLOT_AUTO_SAVE_FILE_NAME] =
                    crate::emacs_core::value::Value::heap_string(load_lisp_string(asname));
            } else if let Some(ref asname) = db.auto_save_file_name {
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
    let entries: HashMap<SymId, AutoloadEntry> = if dam.entries_syms.is_empty() {
        dam.entries
            .iter()
            .map(|(k, e)| {
                (
                    crate::emacs_core::intern::intern(k),
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
            .collect()
    } else {
        dam.entries_syms
            .iter()
            .map(|(k, e)| {
                (
                    load_sym_id(k),
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
            .collect()
    };
    let after_load: HashMap<crate::emacs_core::autoload::AfterLoadKey, Vec<Value>> =
        if !dam.after_load_lisp.is_empty() {
            dam.after_load_lisp
                .iter()
                .map(|(k, v)| {
                    (
                        crate::emacs_core::autoload::AfterLoadKey::from_lisp_string(
                            &load_lisp_string(k),
                        ),
                        v.iter().map(|value| decoder.load_value(value)).collect(),
                    )
                })
                .collect()
        } else {
            dam.after_load
                .iter()
                .map(|(k, v)| {
                    (
                        crate::emacs_core::autoload::AfterLoadKey::from_runtime(k),
                        v.iter().map(|value| decoder.load_value(value)).collect(),
                    )
                })
                .collect()
        };
    AutoloadManager::from_dump(
        entries,
        after_load,
        dam.loaded_files.iter().map(load_lisp_string).collect(),
        if dam.obsolete_functions_syms.is_empty() {
            dam.obsolete_functions
                .iter()
                .map(|(k, (new_name, when))| {
                    (
                        crate::emacs_core::intern::intern(k),
                        (
                            crate::emacs_core::builtins::runtime_string_to_lisp_string(
                                new_name, true,
                            ),
                            crate::emacs_core::builtins::runtime_string_to_lisp_string(when, true),
                        ),
                    )
                })
                .collect()
        } else {
            dam.obsolete_functions_syms
                .iter()
                .map(|(k, (new_name, when))| {
                    (
                        load_sym_id(k),
                        (load_lisp_string(new_name), load_lisp_string(when)),
                    )
                })
                .collect()
        },
        if dam.obsolete_variables_syms.is_empty() {
            dam.obsolete_variables
                .iter()
                .map(|(k, (new_name, when))| {
                    (
                        crate::emacs_core::intern::intern(k),
                        (
                            crate::emacs_core::builtins::runtime_string_to_lisp_string(
                                new_name, true,
                            ),
                            crate::emacs_core::builtins::runtime_string_to_lisp_string(when, true),
                        ),
                    )
                })
                .collect()
        } else {
            dam.obsolete_variables_syms
                .iter()
                .map(|(k, (new_name, when))| {
                    (
                        load_sym_id(k),
                        (load_lisp_string(new_name), load_lisp_string(when)),
                    )
                })
                .collect()
        },
    )
}

pub(crate) fn load_custom_manager(_dcm: &DumpCustomManager) -> CustomManager {
    // Phase D: auto_buffer_local was a pure mirror of LOCALIZED BLV
    // local_if_set flags. Those are restored when symbols are loaded
    // from the dump via their BLV state. No runtime set needed.
    CustomManager {}
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
    let major_modes: HashMap<SymId, MajorMode> = dmr
        .major_modes
        .iter()
        .map(|(k, m)| {
            (
                load_sym_id(k),
                MajorMode {
                    pretty_name: load_lisp_string(&m.pretty_name),
                    parent: decoder.load_opt_value(&m.parent),
                    mode_hook: decoder.load_value(&m.mode_hook),
                    keymap_name: decoder.load_opt_value(&m.keymap_name),
                    syntax_table_name: decoder.load_opt_value(&m.syntax_table_name),
                    abbrev_table_name: decoder.load_opt_value(&m.abbrev_table_name),
                    font_lock: m.font_lock.as_ref().map(|fl| FontLockDefaults {
                        keywords: fl
                            .keywords
                            .iter()
                            .map(|kw| FontLockKeyword {
                                pattern: kw
                                    .pattern_lisp
                                    .as_ref()
                                    .map(load_lisp_string)
                                    .unwrap_or_else(|| {
                                        LispString::from_utf8(
                                            kw.pattern.as_deref().unwrap_or_default(),
                                        )
                                    }),
                                face: kw.face_sym.as_ref().map(load_sym_id).unwrap_or_else(|| {
                                    crate::emacs_core::intern::intern(
                                        kw.face.as_deref().unwrap_or_default(),
                                    )
                                }),
                                group: kw.group,
                                override_: kw.override_,
                                laxmatch: kw.laxmatch,
                            })
                            .collect(),
                        case_fold: fl.case_fold,
                        syntax_table: fl
                            .syntax_table_lisp
                            .as_ref()
                            .map(load_lisp_string)
                            .or_else(|| fl.syntax_table.as_deref().map(LispString::from_utf8)),
                    }),
                    body: decoder.load_opt_value(&m.body),
                },
            )
        })
        .collect();
    let minor_modes: HashMap<SymId, MinorMode> = dmr
        .minor_modes
        .iter()
        .map(|(k, m)| {
            (
                load_sym_id(k),
                MinorMode {
                    lighter: m.lighter.as_ref().map(load_lisp_string),
                    keymap_name: decoder.load_opt_value(&m.keymap_name),
                    global: m.global,
                    body: decoder.load_opt_value(&m.body),
                },
            )
        })
        .collect();
    let custom_variables: HashMap<SymId, ModeCustomVariable> = dmr
        .custom_variables
        .iter()
        .map(|(k, cv)| {
            (
                load_sym_id(k),
                ModeCustomVariable {
                    default_value: decoder.load_value(&cv.default_value),
                    doc: cv.doc.as_ref().map(load_lisp_string),
                    type_: load_mode_custom_type(decoder, &cv.custom_type),
                    group: decoder.load_opt_value(&cv.group),
                    set_function: decoder.load_opt_value(&cv.set_function),
                    get_function: decoder.load_opt_value(&cv.get_function),
                    tag: cv.tag.as_ref().map(load_lisp_string),
                },
            )
        })
        .collect();
    let custom_groups: HashMap<SymId, ModeCustomGroup> = dmr
        .custom_groups
        .iter()
        .map(|(k, g)| {
            (
                load_sym_id(k),
                ModeCustomGroup {
                    doc: g.doc.as_ref().map(load_lisp_string),
                    parent: decoder.load_opt_value(&g.parent),
                    members: g
                        .members
                        .iter()
                        .map(|value| decoder.load_value(value))
                        .collect(),
                },
            )
        })
        .collect();
    ModeRegistry::from_dump(
        major_modes,
        minor_modes,
        dmr.buffer_major_modes
            .iter()
            .map(|(k, v)| (*k, decoder.load_value(v)))
            .collect(),
        dmr.buffer_minor_modes
            .iter()
            .map(|(k, v)| {
                (
                    *k,
                    v.iter().map(|value| decoder.load_value(value)).collect(),
                )
            })
            .collect(),
        dmr.global_minor_modes
            .iter()
            .map(|value| decoder.load_value(value))
            .collect(),
        if !dmr.auto_mode_alist_lisp.is_empty() {
            dmr.auto_mode_alist_lisp
                .iter()
                .map(|(pattern, value)| (load_lisp_string(pattern), decoder.load_value(value)))
                .collect()
        } else {
            dmr.auto_mode_alist
                .iter()
                .map(|(pattern, value)| (LispString::from_utf8(pattern), decoder.load_value(value)))
                .collect()
        },
        custom_variables,
        custom_groups,
        decoder.load_value(&dmr.fundamental_mode),
    )
}

pub(crate) fn load_coding_system_manager(
    decoder: &mut LoadDecoder,
    dcsm: &DumpCodingSystemManager,
) -> CodingSystemManager {
    let systems: HashMap<SymId, CodingSystemInfo> = if dcsm.systems_syms.is_empty() {
        dcsm.systems
            .iter()
            .map(|(k, v)| {
                (
                    crate::emacs_core::intern::intern(k),
                    CodingSystemInfo {
                        name: crate::emacs_core::intern::intern(
                            v.name
                                .as_deref()
                                .expect("legacy coding dump entry missing name"),
                        ),
                        coding_type: crate::emacs_core::intern::intern(
                            v.coding_type
                                .as_deref()
                                .expect("legacy coding dump entry missing coding type"),
                        ),
                        mnemonic: v.mnemonic,
                        eol_type: match v.eol_type {
                            DumpEolType::Unix => EolType::Unix,
                            DumpEolType::Dos => EolType::Dos,
                            DumpEolType::Mac => EolType::Mac,
                            DumpEolType::Undecided => EolType::Undecided,
                        },
                        ascii_compatible_p: v.ascii_compatible_p,
                        charset_list: v
                            .charset_list
                            .iter()
                            .map(|name| crate::emacs_core::intern::intern(name))
                            .collect(),
                        post_read_conversion: v
                            .post_read_conversion
                            .as_ref()
                            .map(|name| crate::emacs_core::intern::intern(name)),
                        pre_write_conversion: v
                            .pre_write_conversion
                            .as_ref()
                            .map(|name| crate::emacs_core::intern::intern(name)),
                        default_char: v.default_char,
                        for_unibyte: v.for_unibyte,
                        properties: v
                            .properties
                            .iter()
                            .map(|(k, v)| {
                                (crate::emacs_core::intern::intern(k), decoder.load_value(v))
                            })
                            .collect(),
                        int_properties: v
                            .int_properties
                            .iter()
                            .map(|(k, v)| (*k, decoder.load_value(v)))
                            .collect(),
                    },
                )
            })
            .collect()
    } else {
        dcsm.systems_syms
            .iter()
            .map(|(k, v)| {
                (
                    load_sym_id(k),
                    CodingSystemInfo {
                        name: v.name_sym.as_ref().map(load_sym_id).unwrap_or_else(|| {
                            crate::emacs_core::intern::intern(
                                v.name.as_deref().expect("coding dump entry missing name"),
                            )
                        }),
                        coding_type: v.coding_type_sym.as_ref().map(load_sym_id).unwrap_or_else(
                            || {
                                crate::emacs_core::intern::intern(
                                    v.coding_type
                                        .as_deref()
                                        .expect("coding dump entry missing coding type"),
                                )
                            },
                        ),
                        mnemonic: v.mnemonic,
                        eol_type: match v.eol_type {
                            DumpEolType::Unix => EolType::Unix,
                            DumpEolType::Dos => EolType::Dos,
                            DumpEolType::Mac => EolType::Mac,
                            DumpEolType::Undecided => EolType::Undecided,
                        },
                        ascii_compatible_p: v.ascii_compatible_p,
                        charset_list: if v.charset_list_syms.is_empty() {
                            v.charset_list
                                .iter()
                                .map(|name| crate::emacs_core::intern::intern(name))
                                .collect()
                        } else {
                            v.charset_list_syms.iter().map(load_sym_id).collect()
                        },
                        post_read_conversion: v
                            .post_read_conversion_sym
                            .as_ref()
                            .map(load_sym_id)
                            .or_else(|| {
                                v.post_read_conversion
                                    .as_ref()
                                    .map(|name| crate::emacs_core::intern::intern(name))
                            }),
                        pre_write_conversion: v
                            .pre_write_conversion_sym
                            .as_ref()
                            .map(load_sym_id)
                            .or_else(|| {
                                v.pre_write_conversion
                                    .as_ref()
                                    .map(|name| crate::emacs_core::intern::intern(name))
                            }),
                        default_char: v.default_char,
                        for_unibyte: v.for_unibyte,
                        properties: if v.properties_syms.is_empty() {
                            v.properties
                                .iter()
                                .map(|(k, v)| {
                                    (crate::emacs_core::intern::intern(k), decoder.load_value(v))
                                })
                                .collect()
                        } else {
                            v.properties_syms
                                .iter()
                                .map(|(k, v)| (load_sym_id(k), decoder.load_value(v)))
                                .collect()
                        },
                        int_properties: v
                            .int_properties
                            .iter()
                            .map(|(k, v)| (*k, decoder.load_value(v)))
                            .collect(),
                    },
                )
            })
            .collect()
    };
    CodingSystemManager::from_dump(
        systems,
        if dcsm.aliases_syms.is_empty() {
            dcsm.aliases
                .iter()
                .map(|(k, v)| {
                    (
                        crate::emacs_core::intern::intern(k),
                        crate::emacs_core::intern::intern(v),
                    )
                })
                .collect()
        } else {
            dcsm.aliases_syms
                .iter()
                .map(|(k, v)| (load_sym_id(k), load_sym_id(v)))
                .collect()
        },
        if dcsm.priority_syms.is_empty() {
            dcsm.priority
                .iter()
                .map(|name| crate::emacs_core::intern::intern(name))
                .collect()
        } else {
            dcsm.priority_syms.iter().map(load_sym_id).collect()
        },
        dcsm.keyboard_coding_sym
            .as_ref()
            .map(load_sym_id)
            .unwrap_or_else(|| {
                crate::emacs_core::intern::intern(
                    dcsm.keyboard_coding
                        .as_deref()
                        .expect("legacy coding dump missing keyboard coding"),
                )
            }),
        dcsm.terminal_coding_sym
            .as_ref()
            .map(load_sym_id)
            .unwrap_or_else(|| {
                crate::emacs_core::intern::intern(
                    dcsm.terminal_coding
                        .as_deref()
                        .expect("legacy coding dump missing terminal coding"),
                )
            }),
    )
}

pub(crate) fn load_charset_registry(decoder: &mut LoadDecoder, dcr: &DumpCharsetRegistry) {
    let snapshot = CharsetRegistrySnapshot {
        charsets: dcr
            .charsets
            .iter()
            .map(|info| CharsetInfoSnapshot {
                id: info.id,
                name: info.name_sym.as_ref().map(load_sym_id).unwrap_or_else(|| {
                    crate::emacs_core::intern::intern(
                        info.name
                            .as_deref()
                            .expect("legacy charset dump entry missing name"),
                    )
                }),
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
                unify_map: decoder.load_value(&info.unify_map),
                method: match &info.method {
                    DumpCharsetMethod::Offset(offset) => CharsetMethodSnapshot::Offset(*offset),
                    DumpCharsetMethod::Map(map_name) => {
                        CharsetMethodSnapshot::Map(map_name.clone())
                    }
                    DumpCharsetMethod::Subset(subset) => CharsetMethodSnapshot::Subset(
                        crate::emacs_core::charset::CharsetSubsetSpecSnapshot {
                            parent: subset.parent_sym.as_ref().map(load_sym_id).unwrap_or_else(
                                || {
                                    crate::emacs_core::intern::intern(
                                        subset
                                            .parent
                                            .as_deref()
                                            .expect("legacy charset subset missing parent"),
                                    )
                                },
                            ),
                            parent_min_code: subset.parent_min_code,
                            parent_max_code: subset.parent_max_code,
                            offset: subset.offset,
                        },
                    ),
                    DumpCharsetMethod::SupersetSyms(members) => CharsetMethodSnapshot::Superset(
                        members
                            .iter()
                            .map(|(name, offset)| (load_sym_id(name), *offset))
                            .collect(),
                    ),
                    DumpCharsetMethod::Superset(members) => CharsetMethodSnapshot::Superset(
                        members
                            .iter()
                            .map(|(name, offset)| {
                                (crate::emacs_core::intern::intern(name), *offset)
                            })
                            .collect(),
                    ),
                },
                plist: if info.plist_syms.is_empty() {
                    info.plist
                        .iter()
                        .map(|(key, value)| {
                            (
                                crate::emacs_core::intern::intern(key),
                                decoder.load_value(value),
                            )
                        })
                        .collect()
                } else {
                    info.plist_syms
                        .iter()
                        .map(|(key, value)| (load_sym_id(key), decoder.load_value(value)))
                        .collect()
                },
            })
            .collect(),
        priority: if dcr.priority_syms.is_empty() {
            dcr.priority
                .iter()
                .map(|name| crate::emacs_core::intern::intern(name))
                .collect()
        } else {
            dcr.priority_syms.iter().map(load_sym_id).collect()
        },
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
        DumpFontRepertory::Charset(name) => {
            FontRepertory::Charset(crate::emacs_core::intern::intern(name))
        }
        DumpFontRepertory::CharsetSym(name) => FontRepertory::Charset(load_sym_id(name)),
        DumpFontRepertory::CharTableRanges(ranges) => {
            FontRepertory::CharTableRanges(ranges.clone())
        }
    }
}

fn load_font_spec_entry(entry: &DumpFontSpecEntry) -> FontSpecEntry {
    match entry {
        DumpFontSpecEntry::Font(spec) => FontSpecEntry::Font(StoredFontSpec {
            family: spec.family_sym.as_ref().map(load_sym_id).or_else(|| {
                spec.family
                    .as_deref()
                    .map(crate::emacs_core::intern::intern)
            }),
            registry: spec.registry_sym.as_ref().map(load_sym_id).or_else(|| {
                spec.registry
                    .as_deref()
                    .map(crate::emacs_core::intern::intern)
            }),
            lang: spec
                .lang_sym
                .as_ref()
                .map(load_sym_id)
                .or_else(|| spec.lang.as_deref().map(crate::emacs_core::intern::intern)),
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
        ordered_names: if dfr.ordered_names_lisp.is_empty() {
            dfr.ordered_names
                .iter()
                .map(|name| LispString::from_utf8(name))
                .collect()
        } else {
            dfr.ordered_names_lisp
                .iter()
                .map(load_lisp_string)
                .collect()
        },
        alias_to_name: if dfr.alias_to_name_lisp.is_empty() {
            dfr.alias_to_name
                .iter()
                .map(|(alias, name)| (LispString::from_utf8(alias), LispString::from_utf8(name)))
                .collect()
        } else {
            dfr.alias_to_name_lisp
                .iter()
                .map(|(alias, name)| (load_lisp_string(alias), load_lisp_string(name)))
                .collect()
        },
        fontsets: if dfr.fontsets_lisp.is_empty() {
            dfr.fontsets
                .iter()
                .map(|(name, data)| {
                    (
                        LispString::from_utf8(name),
                        FontsetDataSnapshot {
                            ranges: data
                                .ranges
                                .iter()
                                .map(|range| FontsetRangeEntrySnapshot {
                                    from: range.from,
                                    to: range.to,
                                    entries: range
                                        .entries
                                        .iter()
                                        .map(load_font_spec_entry)
                                        .collect(),
                                })
                                .collect(),
                            fallback: data
                                .fallback
                                .as_ref()
                                .map(|entries| entries.iter().map(load_font_spec_entry).collect()),
                        },
                    )
                })
                .collect()
        } else {
            dfr.fontsets_lisp
                .iter()
                .map(|(name, data)| {
                    (
                        load_lisp_string(name),
                        FontsetDataSnapshot {
                            ranges: data
                                .ranges
                                .iter()
                                .map(|range| FontsetRangeEntrySnapshot {
                                    from: range.from,
                                    to: range.to,
                                    entries: range
                                        .entries
                                        .iter()
                                        .map(load_font_spec_entry)
                                        .collect(),
                                })
                                .collect(),
                            fallback: data
                                .fallback
                                .as_ref()
                                .map(|entries| entries.iter().map(load_font_spec_entry).collect()),
                        },
                    )
                })
                .collect()
        },
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

fn load_face(decoder: &mut LoadDecoder, df: &DumpFace) -> Face {
    Face {
        foreground: df.foreground.map(|c| load_color(&c)),
        background: df.background.map(|c| load_color(&c)),
        family: df
            .family_value
            .as_ref()
            .map(|value| decoder.load_value(value))
            .or_else(|| df.family.as_ref().map(Value::string)),
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
        stipple: df
            .stipple_value
            .as_ref()
            .map(|value| decoder.load_value(value))
            .or_else(|| df.stipple.as_ref().map(Value::string)),
        extend: df.extend,
        inherit: {
            // Dump legacy schema: Vec<symbol-name>. Reconstruct as a
            // single symbol if exactly one, or a face_ref list otherwise,
            // matching GNU's LFACE_INHERIT_INDEX value shape.
            let syms: Vec<Value> = if !df.inherit_syms.is_empty() {
                df.inherit_syms
                    .iter()
                    .map(|name| Value::from_sym_id(load_sym_id(name)))
                    .collect()
            } else {
                df.inherit
                    .iter()
                    .map(|name| Value::symbol(name.as_str()))
                    .collect()
            };
            match syms.len() {
                0 => None,
                1 => Some(syms[0]),
                _ => Some(Value::list(syms)),
            }
        },
        overstrike: df.overstrike,
        doc: df
            .doc_value
            .as_ref()
            .map(|value| decoder.load_value(value))
            .or_else(|| df.doc.as_ref().map(Value::string)),
        overline_color: None,
        strike_through_color: None,
        distant_foreground: None,
        foundry: df
            .foundry_value
            .as_ref()
            .map(|value| decoder.load_value(value))
            .or_else(|| df.foundry.as_ref().map(Value::string)),
        width: None,
    }
}

pub(crate) fn load_face_table(decoder: &mut LoadDecoder, dft: &DumpFaceTable) -> FaceTable {
    if !dft.face_ids.is_empty() {
        FaceTable::from_dump_sym_ids(
            dft.face_ids
                .iter()
                .map(|(k, f)| (load_sym_id(k), load_face(decoder, f)))
                .collect(),
        )
    } else {
        FaceTable::from_dump(
            dft.faces
                .iter()
                .map(|(k, f)| (k.clone(), load_face(decoder, f)))
                .collect(),
        )
    }
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
        counter_format: dkm
            .counter_format_lisp
            .as_ref()
            .map(load_lisp_string)
            .or_else(|| {
                dkm.counter_format.as_ref().map(|text| {
                    crate::emacs_core::builtins::runtime_string_to_lisp_string(text, true)
                })
            })
            .unwrap_or_else(|| crate::heap_types::LispString::from_utf8("%d")),
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
    let bookmarks: HashMap<crate::emacs_core::bookmark::BookmarkKey, Bookmark> =
        if !dbm.bookmarks_lisp.is_empty() {
            dbm.bookmarks_lisp
                .iter()
                .map(|(k, b)| {
                    (
                        crate::emacs_core::bookmark::BookmarkKey::from_lisp_string(
                            &load_lisp_string(k),
                        ),
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
                .collect()
        } else {
            dbm.bookmarks
                .iter()
                .map(|(k, b)| {
                    (
                        crate::emacs_core::bookmark::BookmarkKey::from_lisp_string(
                            &crate::emacs_core::builtins::runtime_string_to_lisp_string(k, true),
                        ),
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
                .collect()
        };
    BookmarkManager::from_dump(bookmarks, dbm.recent.iter().map(load_lisp_string).collect())
}

pub(crate) fn load_abbrev_manager(dam: &DumpAbbrevManager) -> AbbrevManager {
    let tables: HashMap<SymId, AbbrevTable> = if !dam.tables_syms.is_empty() {
        dam.tables_syms
            .iter()
            .map(|(sym, t)| {
                (
                    load_sym_id(sym),
                    AbbrevTable {
                        name: load_lisp_string(&t.name),
                        abbrevs: t
                            .abbrevs
                            .iter()
                            .map(|(k, a)| {
                                (
                                    load_lisp_string(k),
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
            .collect()
    } else {
        dam.tables
            .iter()
            .map(|(k, t)| {
                (
                    intern::intern(k),
                    AbbrevTable {
                        name: load_lisp_string(&t.name),
                        abbrevs: t
                            .abbrevs
                            .iter()
                            .map(|(k, a)| {
                                (
                                    load_lisp_string(k),
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
            .collect()
    };
    let global_table_sym = dam
        .global_table_sym
        .map(|sym| load_sym_id(&sym))
        .unwrap_or_else(|| {
            intern::intern(
                &crate::emacs_core::builtins::runtime_string_from_lisp_string(&load_lisp_string(
                    &dam.global_table_name,
                )),
            )
        });
    AbbrevManager::from_dump(tables, global_table_sym, dam.abbrev_mode)
}

pub(crate) fn load_interactive_registry(
    decoder: &mut LoadDecoder,
    dir: &DumpInteractiveRegistry,
) -> InteractiveRegistry {
    let specs: HashMap<SymId, InteractiveSpec> = dir
        .specs
        .iter()
        .map(|(k, s)| {
            (
                load_sym_id(k),
                InteractiveSpec {
                    spec: decoder.load_value(&s.spec),
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
