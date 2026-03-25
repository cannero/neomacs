//! Conversions between runtime types and pdump snapshot types.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::types::*;
use crate::buffer::buffer::{Buffer, BufferId, BufferManager, InsertionType, MarkerEntry};
use crate::buffer::buffer_text::BufferText;
use crate::buffer::overlay::{Overlay, OverlayList};
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
use crate::emacs_core::custom::{CustomGroup, CustomManager, CustomVariable};
use crate::emacs_core::eval::Context;
use crate::emacs_core::expr::Expr;
use crate::emacs_core::fontset::{
    FontRepertory, FontSpecEntry, FontsetDataSnapshot, FontsetRangeEntrySnapshot,
    FontsetRegistrySnapshot, StoredFontSpec, restore_fontset_registry, snapshot_fontset_registry,
};
use crate::emacs_core::interactive::{InteractiveRegistry, InteractiveSpec};
use crate::emacs_core::intern::{self, StringInterner, SymId};
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
use crate::face::{
    BoxBorder, BoxStyle, Color, Face, FaceHeight, FaceTable, FontSlant, FontWeight, FontWidth,
    Underline, UnderlineStyle,
};
use crate::gc::heap::LispHeap;
use crate::gc::types::{HeapObject, ObjId};

// ===========================================================================
// Dump direction: Runtime → Dump
// ===========================================================================

// --- Primitives ---

pub(crate) fn dump_obj_id(id: ObjId) -> DumpObjId {
    DumpObjId {
        index: id.index,
        generation: id.generation,
    }
}

pub(crate) fn dump_sym_id(id: SymId) -> DumpSymId {
    DumpSymId(id.0)
}

pub(crate) fn dump_value(v: &Value) -> DumpValue {
    match *v {
        Value::Nil => DumpValue::Nil,
        Value::True => DumpValue::True,
        Value::Int(n) => DumpValue::Int(n),
        Value::Float(f, id) => DumpValue::Float(f, id),
        Value::Symbol(s) => DumpValue::Symbol(dump_sym_id(s)),
        Value::Keyword(s) => DumpValue::Keyword(dump_sym_id(s)),
        Value::Str(id) => DumpValue::Str(dump_obj_id(id)),
        Value::Cons(id) => DumpValue::Cons(dump_obj_id(id)),
        Value::Vector(id) => DumpValue::Vector(dump_obj_id(id)),
        Value::Record(id) => DumpValue::Record(dump_obj_id(id)),
        Value::HashTable(id) => DumpValue::HashTable(dump_obj_id(id)),
        Value::Lambda(id) => DumpValue::Lambda(dump_obj_id(id)),
        Value::Macro(id) => DumpValue::Macro(dump_obj_id(id)),
        Value::Char(c) => DumpValue::Char(c),
        Value::Subr(s) => DumpValue::Subr(dump_sym_id(s)),
        Value::ByteCode(id) => DumpValue::ByteCode(dump_obj_id(id)),
        Value::Buffer(bid) => DumpValue::Buffer(DumpBufferId(bid.0)),
        Value::Window(w) => DumpValue::Window(w),
        Value::Frame(f) => DumpValue::Frame(f),
        Value::Timer(t) => DumpValue::Timer(t),
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
        Expr::OpaqueValue(v) => DumpExpr::OpaqueValue(dump_value(v)),
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
        Op::UnwindProtect(n) => DumpOp::UnwindProtect(n),
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

pub(crate) fn dump_lambda_data(d: &LambdaData) -> DumpLambdaData {
    DumpLambdaData {
        params: dump_lambda_params(&d.params),
        body: d.body.iter().map(dump_expr).collect(),
        env: dump_opt_value(&d.env),
        docstring: d.docstring.clone(),
        doc_form: dump_opt_value(&d.doc_form),
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
        HashKey::Str(id) => DumpHashKey::Str(dump_obj_id(*id)),
        HashKey::Char(c) => DumpHashKey::Char(*c),
        HashKey::Window(w) => DumpHashKey::Window(*w),
        HashKey::Frame(f) => DumpHashKey::Frame(*f),
        HashKey::Ptr(p) => DumpHashKey::Ptr(*p as u64),
        HashKey::ObjId(a, b) => DumpHashKey::ObjId(*a, *b),
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

pub(crate) fn dump_heap_object(obj: &HeapObject) -> DumpHeapObject {
    match obj {
        HeapObject::Cons { car, cdr } => DumpHeapObject::Cons {
            car: dump_value(car),
            cdr: dump_value(cdr),
        },
        HeapObject::Vector(items) => DumpHeapObject::Vector(items.iter().map(dump_value).collect()),
        HeapObject::HashTable(ht) => DumpHeapObject::HashTable(dump_hash_table(ht)),
        HeapObject::Str(s) => DumpHeapObject::Str {
            text: s.as_str().to_owned(),
            multibyte: s.multibyte,
        },
        HeapObject::Lambda(d) => DumpHeapObject::Lambda(dump_lambda_data(d)),
        HeapObject::Macro(d) => DumpHeapObject::Macro(dump_lambda_data(d)),
        HeapObject::ByteCode(bc) => DumpHeapObject::ByteCode(dump_bytecode(bc)),
        HeapObject::Free => DumpHeapObject::Free,
    }
}

// --- Heap ---

pub(crate) fn dump_heap(heap: &LispHeap) -> DumpLispHeap {
    DumpLispHeap {
        objects: heap.objects().iter().map(dump_heap_object).collect(),
        generations: heap.generations().to_vec(),
        free_list: heap.free_list().to_vec(),
    }
}

// --- Interner ---

pub(crate) fn dump_interner(interner: &StringInterner) -> DumpStringInterner {
    DumpStringInterner {
        strings: interner.strings().to_vec(),
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
            .iter()
            .map(dump_property_interval)
            .collect(),
    }
}

fn dump_overlay(o: &Overlay) -> DumpOverlay {
    DumpOverlay {
        id: o.id,
        start: o.start,
        end: o.end,
        properties: o
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), dump_value(v)))
            .collect(),
        front_advance: o.front_advance,
        rear_advance: o.rear_advance,
    }
}

fn dump_overlay_list(ol: &OverlayList) -> DumpOverlayList {
    DumpOverlayList {
        overlays: ol.dump_overlays().iter().map(dump_overlay).collect(),
        next_id: ol.dump_next_id(),
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
        SyntaxClass::Prefix => DumpSyntaxClass::Prefix,
        SyntaxClass::StringDelim => DumpSyntaxClass::StringDelim,
        SyntaxClass::MathDelim => DumpSyntaxClass::MathDelim,
        SyntaxClass::Escape => DumpSyntaxClass::Escape,
        SyntaxClass::CharQuote => DumpSyntaxClass::CharQuote,
        SyntaxClass::Comment => DumpSyntaxClass::Comment,
        SyntaxClass::EndComment => DumpSyntaxClass::EndComment,
        SyntaxClass::InheritStandard => DumpSyntaxClass::InheritStandard,
        SyntaxClass::Generic => DumpSyntaxClass::Generic,
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
        read_only: buf.read_only,
        multibyte: buf.multibyte,
        file_name: buf.file_name.clone(),
        auto_save_file_name: buf.auto_save_file_name.clone(),
        markers: buf.markers.iter().map(dump_marker).collect(),
        properties: buf
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), dump_runtime_binding_value(v)))
            .collect(),
        local_map: dump_value(&buf.local_map),
        text_props: dump_text_property_table(&buf.text_props),
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
        variables: cm
            .variables
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    DumpCustomVariable {
                        name: v.name.clone(),
                        custom_type: dump_value(&v.custom_type),
                        group: v.group.clone(),
                        documentation: v.documentation.clone(),
                        standard_value: dump_value(&v.standard_value),
                        set_function: dump_opt_value(&v.set_function),
                        get_function: dump_opt_value(&v.get_function),
                        initialize: dump_opt_value(&v.initialize),
                    },
                )
            })
            .collect(),
        groups: cm
            .groups
            .iter()
            .map(|(k, g)| {
                (
                    k.clone(),
                    DumpCustomGroup {
                        name: g.name.clone(),
                        members: g
                            .members
                            .iter()
                            .map(|(n, v)| (n.clone(), dump_value(v)))
                            .collect(),
                        documentation: g.documentation.clone(),
                        parent: g.parent.clone(),
                    },
                )
            })
            .collect(),
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

pub(crate) fn dump_category_manager(
    cm: &crate::emacs_core::category::CategoryManager,
) -> DumpCategoryManager {
    DumpCategoryManager {
        tables: cm
            .tables
            .iter()
            .map(|(k, t)| {
                (
                    k.clone(),
                    DumpCategoryTable {
                        entries: t
                            .entries
                            .iter()
                            .map(|(c, set)| (*c, set.iter().cloned().collect()))
                            .collect(),
                        descriptions: t
                            .descriptions
                            .iter()
                            .map(|(c, s)| (*c, s.clone()))
                            .collect(),
                    },
                )
            })
            .collect(),
        current_table: cm.current_table.clone(),
    }
}

pub(crate) fn dump_rectangle(r: &RectangleState) -> DumpRectangleState {
    DumpRectangleState {
        killed: r.killed.clone(),
    }
}

pub(crate) fn dump_kmacro(km: &KmacroManager) -> DumpKmacroManager {
    DumpKmacroManager {
        current_macro: km.current_macro.iter().map(dump_value).collect(),
        last_macro: km
            .last_macro
            .as_ref()
            .map(|m| m.iter().map(dump_value).collect()),
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
    let string_text_props = crate::emacs_core::value::snapshot_string_text_props();
    DumpContextState {
        interner: dump_interner(&eval.interner),
        heap: dump_heap(&eval.heap),
        obarray: dump_obarray(&eval.obarray),
        dynamic: eval.dynamic.iter().map(dump_ordered_sym_map).collect(),
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
        category_manager: dump_category_manager(&eval.category_manager),
        abbrevs: dump_abbrev_manager(&eval.abbrevs),
        interactive: dump_interactive_registry(&eval.interactive),
        rectangle: dump_rectangle(&eval.rectangle),
        standard_syntax_table: dump_value(&eval.standard_syntax_table),
        current_local_map: dump_value(&eval.current_local_map),
        kmacro: dump_kmacro(&eval.kmacro),
        registers: dump_register_manager(&eval.registers),
        bookmarks: dump_bookmark_manager(&eval.bookmarks),
        watchers: dump_watcher_list(&eval.watchers),
        string_text_props: string_text_props
            .into_iter()
            .map(|(key, table)| (key, dump_string_text_property_table(&table)))
            .collect(),
    }
}

// ===========================================================================
// Load direction: Dump → Runtime
// ===========================================================================

// --- Primitives ---

pub(crate) fn load_obj_id(id: &DumpObjId) -> ObjId {
    ObjId {
        index: id.index,
        generation: id.generation,
    }
}

pub(crate) fn load_sym_id(id: &DumpSymId) -> SymId {
    SymId(id.0)
}

pub(crate) fn load_value(v: &DumpValue) -> Value {
    match v {
        DumpValue::Nil => Value::Nil,
        DumpValue::True => Value::True,
        DumpValue::Int(n) => Value::Int(*n),
        DumpValue::Float(f, id) => Value::Float(*f, *id),
        DumpValue::Symbol(s) => Value::Symbol(load_sym_id(s)),
        DumpValue::Keyword(s) => Value::Keyword(load_sym_id(s)),
        DumpValue::Str(id) => Value::Str(load_obj_id(id)),
        DumpValue::Cons(id) => Value::Cons(load_obj_id(id)),
        DumpValue::Vector(id) => Value::Vector(load_obj_id(id)),
        DumpValue::Record(id) => Value::Record(load_obj_id(id)),
        DumpValue::HashTable(id) => Value::HashTable(load_obj_id(id)),
        DumpValue::Lambda(id) => Value::Lambda(load_obj_id(id)),
        DumpValue::Macro(id) => Value::Macro(load_obj_id(id)),
        DumpValue::Char(c) => Value::Char(*c),
        DumpValue::Subr(s) => Value::Subr(load_sym_id(s)),
        DumpValue::ByteCode(id) => Value::ByteCode(load_obj_id(id)),
        DumpValue::Buffer(bid) => Value::Buffer(BufferId(bid.0)),
        DumpValue::Window(w) => Value::Window(*w),
        DumpValue::Frame(f) => Value::Frame(*f),
        DumpValue::Timer(t) => Value::Timer(*t),
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
        DumpExpr::OpaqueValue(v) => Expr::OpaqueValue(load_value(v)),
    }
}

// --- Op ---

pub(crate) fn load_op(op: &DumpOp) -> Op {
    match *op {
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
        DumpOp::UnwindProtect(n) => Op::UnwindProtect(n),
        DumpOp::UnwindProtectPop => Op::UnwindProtectPop,
        DumpOp::Throw => Op::Throw,
        DumpOp::SaveCurrentBuffer => Op::SaveCurrentBuffer,
        DumpOp::SaveExcursion => Op::SaveExcursion,
        DumpOp::SaveRestriction => Op::SaveRestriction,
        DumpOp::MakeClosure(n) => Op::MakeClosure(n),
        DumpOp::CallBuiltin(a, b) => Op::CallBuiltin(a, b),
    }
}

// --- Lambda / ByteCode ---

pub(crate) fn load_lambda_params(p: &DumpLambdaParams) -> LambdaParams {
    LambdaParams {
        required: p.required.iter().map(|s| load_sym_id(s)).collect(),
        optional: p.optional.iter().map(|s| load_sym_id(s)).collect(),
        rest: p.rest.map(|s| load_sym_id(&s)),
    }
}

pub(crate) fn load_lambda_data(d: &DumpLambdaData) -> LambdaData {
    LambdaData {
        params: load_lambda_params(&d.params),
        body: Rc::new(d.body.iter().map(load_expr).collect()),
        env: load_opt_value(&d.env),
        docstring: d.docstring.clone(),
        doc_form: load_opt_value(&d.doc_form),
    }
}

pub(crate) fn load_bytecode(bc: &DumpByteCodeFunction) -> ByteCodeFunction {
    ByteCodeFunction {
        ops: bc.ops.iter().map(load_op).collect(),
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
    }
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
        DumpHashKey::Str(id) => HashKey::Str(load_obj_id(id)),
        DumpHashKey::Char(c) => HashKey::Char(*c),
        DumpHashKey::Window(w) => HashKey::Window(*w),
        DumpHashKey::Frame(f) => HashKey::Frame(*f),
        DumpHashKey::Ptr(p) => HashKey::Ptr(*p as usize),
        DumpHashKey::ObjId(a, b) => HashKey::ObjId(*a, *b),
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

// --- Heap objects ---

/// Load a heap object, but defer hash table population.
/// Hash tables need CURRENT_HEAP set for HashKey::Str hashing,
/// so we create empty placeholders first, then populate after heap is set.
fn load_heap_object_phase1(obj: &DumpHeapObject) -> HeapObject {
    match obj {
        DumpHeapObject::Cons { car, cdr } => HeapObject::Cons {
            car: load_value(car),
            cdr: load_value(cdr),
        },
        DumpHeapObject::Vector(items) => HeapObject::Vector(items.iter().map(load_value).collect()),
        DumpHeapObject::HashTable(ht) => {
            // Create empty hash table with correct metadata; entries populated in phase 2
            HeapObject::HashTable(LispHashTable {
                test: load_hash_table_test(&ht.test),
                test_name: ht.test_name.map(|s| load_sym_id(&s)),
                size: ht.size,
                weakness: ht.weakness.as_ref().map(load_hash_table_weakness),
                rehash_size: ht.rehash_size,
                rehash_threshold: ht.rehash_threshold,
                data: HashMap::new(),
                key_snapshots: HashMap::new(),
                insertion_order: Vec::new(),
            })
        }
        DumpHeapObject::Str { text, multibyte } => {
            HeapObject::Str(crate::gc::types::LispString::new(text.clone(), *multibyte))
        }
        DumpHeapObject::Lambda(d) => HeapObject::Lambda(load_lambda_data(d)),
        DumpHeapObject::Macro(d) => HeapObject::Macro(load_lambda_data(d)),
        DumpHeapObject::ByteCode(bc) => HeapObject::ByteCode(load_bytecode(bc)),
        DumpHeapObject::Free => HeapObject::Free,
    }
}

// --- Heap ---

/// Load heap in two phases:
/// Phase 1: Create all objects with empty hash tables (no heap access needed)
/// Phase 2: After CURRENT_HEAP is set, populate hash table entries
///          (HashKey::Str hashing requires heap access)
pub(crate) fn load_heap(dh: &DumpLispHeap) -> LispHeap {
    let objects: Vec<HeapObject> = dh.objects.iter().map(load_heap_object_phase1).collect();
    LispHeap::from_dump(objects, dh.generations.clone(), dh.free_list.clone())
}

/// Phase 2: Populate hash table entries after CURRENT_HEAP is set.
pub(crate) fn load_heap_hash_tables(heap: &mut LispHeap, dh: &DumpLispHeap) {
    for (i, obj) in dh.objects.iter().enumerate() {
        if let DumpHeapObject::HashTable(ht) = obj {
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
            let insertion_order: Vec<HashKey> =
                ht.insertion_order.iter().map(load_hash_key).collect();
            if let HeapObject::HashTable(ref mut table) = heap.objects_mut()[i] {
                table.data = data;
                table.key_snapshots = key_snapshots;
                table.insertion_order = insertion_order;
            }
        }
    }
}

// --- Interner ---

pub(crate) fn load_interner(di: &DumpStringInterner) -> StringInterner {
    StringInterner::from_strings(di.strings.clone())
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
        DumpSyntaxClass::Prefix => SyntaxClass::Prefix,
        DumpSyntaxClass::StringDelim => SyntaxClass::StringDelim,
        DumpSyntaxClass::MathDelim => SyntaxClass::MathDelim,
        DumpSyntaxClass::Escape => SyntaxClass::Escape,
        DumpSyntaxClass::CharQuote => SyntaxClass::CharQuote,
        DumpSyntaxClass::Comment => SyntaxClass::Comment,
        DumpSyntaxClass::EndComment => SyntaxClass::EndComment,
        DumpSyntaxClass::InheritStandard => SyntaxClass::InheritStandard,
        DumpSyntaxClass::Generic => SyntaxClass::Generic,
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
    let markers = db
        .markers
        .iter()
        .map(|marker| load_marker(marker, &text))
        .collect();

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
        read_only: db.read_only,
        multibyte: db.multibyte,
        file_name: db.file_name.clone(),
        auto_save_file_name: db.auto_save_file_name.clone(),
        markers,
        properties: db
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), load_runtime_binding_value(v)))
            .collect(),
        local_map: load_value(&db.local_map),
        text_props: TextPropertyTable::from_dump(
            db.text_props
                .intervals
                .iter()
                .map(load_property_interval)
                .collect(),
        ),
        overlays: OverlayList::from_dump(
            db.overlays
                .overlays
                .iter()
                .map(|o| Overlay {
                    id: o.id,
                    start: o.start,
                    end: o.end,
                    properties: o
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), load_value(v)))
                        .collect(),
                    front_advance: o.front_advance,
                    rear_advance: o.rear_advance,
                })
                .collect(),
            db.overlays.next_id,
        ),
        syntax_table: load_syntax_table(&db.syntax_table),
        undo_in_progress: false,
        undo_recorded_first_change: false,
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
        variables: dcm
            .variables
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    CustomVariable {
                        name: v.name.clone(),
                        custom_type: load_value(&v.custom_type),
                        group: v.group.clone(),
                        documentation: v.documentation.clone(),
                        standard_value: load_value(&v.standard_value),
                        set_function: load_opt_value(&v.set_function),
                        get_function: load_opt_value(&v.get_function),
                        initialize: load_opt_value(&v.initialize),
                    },
                )
            })
            .collect(),
        groups: dcm
            .groups
            .iter()
            .map(|(k, g)| {
                (
                    k.clone(),
                    CustomGroup {
                        name: g.name.clone(),
                        members: g
                            .members
                            .iter()
                            .map(|(n, v)| (n.clone(), load_value(v)))
                            .collect(),
                        documentation: g.documentation.clone(),
                        parent: g.parent.clone(),
                    },
                )
            })
            .collect(),
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

pub(crate) fn load_category_manager(
    dcm: &DumpCategoryManager,
) -> crate::emacs_core::category::CategoryManager {
    crate::emacs_core::category::CategoryManager {
        tables: dcm
            .tables
            .iter()
            .map(|(k, t)| {
                (
                    k.clone(),
                    crate::emacs_core::category::CategoryTable {
                        entries: t
                            .entries
                            .iter()
                            .map(|(c, cats)| (*c, cats.iter().cloned().collect()))
                            .collect(),
                        descriptions: t
                            .descriptions
                            .iter()
                            .map(|(c, s)| (*c, s.clone()))
                            .collect(),
                    },
                )
            })
            .collect(),
        current_table: dcm.current_table.clone(),
    }
}

pub(crate) fn load_rectangle(dr: &DumpRectangleState) -> RectangleState {
    RectangleState {
        killed: dr.killed.clone(),
    }
}

pub(crate) fn load_kmacro(dkm: &DumpKmacroManager) -> KmacroManager {
    KmacroManager {
        recording: false,
        executing: false,
        current_macro: dkm.current_macro.iter().map(load_value).collect(),
        last_macro: dkm
            .last_macro
            .as_ref()
            .map(|m| m.iter().map(load_value).collect()),
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
