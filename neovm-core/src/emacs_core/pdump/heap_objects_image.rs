//! Fixed-layout pdump section for heap object descriptors.
//!
//! The raw heap bytes live in the HeapImage section.  This companion section
//! keeps the dump-local object graph out of RuntimeState bincode so file pdumps
//! can load heap identity from mmap-owned sections instead of one monolithic
//! Rust serialization blob.

use bytemuck::{Pod, Zeroable};

use super::DumpError;
use super::types::{
    DumpBufferId, DumpByteCodeFunction, DumpByteData, DumpHashKey, DumpHashTableTest,
    DumpHashTableWeakness, DumpHeapObject, DumpHeapRef, DumpLambdaParams, DumpLispHashTable,
    DumpLispString, DumpMarker, DumpNameId, DumpOp, DumpOverlay, DumpStringTextPropertyRun,
    DumpSymId, DumpValue,
};

const HEAP_OBJECTS_MAGIC: [u8; 16] = *b"NEOHEAPOBJECTS\0\0";
const HEAP_OBJECTS_FORMAT_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct HeapObjectsHeader {
    magic: [u8; 16],
    version: u32,
    header_size: u32,
    object_count: u64,
    payload_offset: u64,
    payload_len: u64,
}

const HEADER_SIZE: usize = std::mem::size_of::<HeapObjectsHeader>();

pub(crate) fn heap_objects_section_bytes(objects: &[DumpHeapObject]) -> Result<Vec<u8>, DumpError> {
    let object_count = u64::try_from(objects.len()).map_err(|_| {
        DumpError::SerializationError("pdump heap object count overflows u64".into())
    })?;
    let mut bytes = vec![0; HEADER_SIZE];
    for object in objects {
        write_heap_object(&mut bytes, object)?;
    }
    let payload_len = bytes.len() - HEADER_SIZE;
    let header = HeapObjectsHeader {
        magic: HEAP_OBJECTS_MAGIC,
        version: HEAP_OBJECTS_FORMAT_VERSION,
        header_size: HEADER_SIZE as u32,
        object_count,
        payload_offset: HEADER_SIZE as u64,
        payload_len: payload_len as u64,
    };
    bytes[..HEADER_SIZE].copy_from_slice(bytemuck::bytes_of(&header));
    Ok(bytes)
}

pub(crate) fn load_heap_objects_section(section: &[u8]) -> Result<Vec<DumpHeapObject>, DumpError> {
    let header = read_header(section)?;
    let payload_offset = usize::try_from(header.payload_offset).map_err(|_| {
        DumpError::ImageFormatError("heap object payload offset overflows usize".into())
    })?;
    let payload_len = usize::try_from(header.payload_len).map_err(|_| {
        DumpError::ImageFormatError("heap object payload length overflows usize".into())
    })?;
    let end = payload_offset
        .checked_add(payload_len)
        .ok_or_else(|| DumpError::ImageFormatError("heap object payload range overflows".into()))?;
    if payload_offset < HEADER_SIZE || end > section.len() {
        return Err(DumpError::ImageFormatError(
            "heap object payload range is outside section".into(),
        ));
    }

    let object_count = usize::try_from(header.object_count)
        .map_err(|_| DumpError::ImageFormatError("heap object count overflows usize".into()))?;
    let mut cursor = Cursor::new(&section[payload_offset..end]);
    let mut objects = Vec::with_capacity(object_count);
    for index in 0..object_count {
        objects.push(cursor.read_heap_object(index)?);
    }
    if !cursor.is_empty() {
        return Err(DumpError::ImageFormatError(format!(
            "heap object section has {} trailing payload bytes",
            cursor.remaining()
        )));
    }
    Ok(objects)
}

fn read_header(section: &[u8]) -> Result<HeapObjectsHeader, DumpError> {
    if section.len() < HEADER_SIZE {
        return Err(DumpError::ImageFormatError(format!(
            "heap object section shorter than header: {} < {HEADER_SIZE}",
            section.len()
        )));
    }
    let header = *bytemuck::from_bytes::<HeapObjectsHeader>(&section[..HEADER_SIZE]);
    if header.magic != HEAP_OBJECTS_MAGIC {
        return Err(DumpError::ImageFormatError(
            "heap object section has bad magic".into(),
        ));
    }
    if header.version != HEAP_OBJECTS_FORMAT_VERSION {
        return Err(DumpError::UnsupportedVersion(header.version));
    }
    if header.header_size != HEADER_SIZE as u32 {
        return Err(DumpError::ImageFormatError(format!(
            "heap object header size {} does not match runtime header size {HEADER_SIZE}",
            header.header_size
        )));
    }
    Ok(header)
}

const HEAP_CONS: u8 = 0;
const HEAP_VECTOR: u8 = 1;
const HEAP_HASH_TABLE: u8 = 2;
const HEAP_STRING: u8 = 3;
const HEAP_FLOAT: u8 = 4;
const HEAP_LAMBDA: u8 = 5;
const HEAP_MACRO: u8 = 6;
const HEAP_BYTE_CODE: u8 = 7;
const HEAP_RECORD: u8 = 8;
const HEAP_MARKER: u8 = 9;
const HEAP_OVERLAY: u8 = 10;
const HEAP_BUFFER: u8 = 11;
const HEAP_WINDOW: u8 = 12;
const HEAP_FRAME: u8 = 13;
const HEAP_TIMER: u8 = 14;
const HEAP_SUBR: u8 = 15;
const HEAP_FREE: u8 = 16;

fn write_heap_object(out: &mut Vec<u8>, object: &DumpHeapObject) -> Result<(), DumpError> {
    match object {
        DumpHeapObject::Cons { car, cdr } => {
            write_u8(out, HEAP_CONS);
            write_value(out, car)?;
            write_value(out, cdr)?;
        }
        DumpHeapObject::Vector(values) => {
            write_u8(out, HEAP_VECTOR);
            write_values(out, values)?;
        }
        DumpHeapObject::HashTable(table) => {
            write_u8(out, HEAP_HASH_TABLE);
            write_hash_table(out, table)?;
        }
        DumpHeapObject::Str {
            data,
            size,
            size_byte,
            text_props,
        } => {
            write_u8(out, HEAP_STRING);
            write_byte_data(out, data)?;
            write_usize(out, *size)?;
            write_i64(out, *size_byte);
            write_text_property_runs(out, text_props)?;
        }
        DumpHeapObject::Float(value) => {
            write_u8(out, HEAP_FLOAT);
            write_f64(out, *value);
        }
        DumpHeapObject::Lambda(values) => {
            write_u8(out, HEAP_LAMBDA);
            write_values(out, values)?;
        }
        DumpHeapObject::Macro(values) => {
            write_u8(out, HEAP_MACRO);
            write_values(out, values)?;
        }
        DumpHeapObject::ByteCode(function) => {
            write_u8(out, HEAP_BYTE_CODE);
            write_byte_code(out, function)?;
        }
        DumpHeapObject::Record(values) => {
            write_u8(out, HEAP_RECORD);
            write_values(out, values)?;
        }
        DumpHeapObject::Marker(marker) => {
            write_u8(out, HEAP_MARKER);
            write_marker(out, marker)?;
        }
        DumpHeapObject::Overlay(overlay) => {
            write_u8(out, HEAP_OVERLAY);
            write_overlay(out, overlay)?;
        }
        DumpHeapObject::Buffer(id) => {
            write_u8(out, HEAP_BUFFER);
            write_u64(out, id.0);
        }
        DumpHeapObject::Window(id) => {
            write_u8(out, HEAP_WINDOW);
            write_u64(out, *id);
        }
        DumpHeapObject::Frame(id) => {
            write_u8(out, HEAP_FRAME);
            write_u64(out, *id);
        }
        DumpHeapObject::Timer(id) => {
            write_u8(out, HEAP_TIMER);
            write_u64(out, *id);
        }
        DumpHeapObject::Subr {
            name,
            min_args,
            max_args,
        } => {
            write_u8(out, HEAP_SUBR);
            write_u32(out, name.0);
            write_u16(out, *min_args);
            write_opt_u16(out, *max_args);
        }
        DumpHeapObject::Free => write_u8(out, HEAP_FREE),
    }
    Ok(())
}

const BYTE_OWNED: u8 = 0;
const BYTE_MAPPED: u8 = 1;

fn write_byte_data(out: &mut Vec<u8>, data: &DumpByteData) -> Result<(), DumpError> {
    match data {
        DumpByteData::Owned(bytes) => {
            write_u8(out, BYTE_OWNED);
            write_bytes(out, bytes)?;
        }
        DumpByteData::Mapped(span) => {
            write_u8(out, BYTE_MAPPED);
            write_u64(out, span.offset);
            write_u64(out, span.len);
        }
    }
    Ok(())
}

const VALUE_NIL: u8 = 0;
const VALUE_TRUE: u8 = 1;
const VALUE_INT: u8 = 2;
const VALUE_FLOAT: u8 = 3;
const VALUE_SYMBOL: u8 = 4;
const VALUE_STR: u8 = 5;
const VALUE_CONS: u8 = 6;
const VALUE_VECTOR: u8 = 7;
const VALUE_RECORD: u8 = 8;
const VALUE_HASH_TABLE: u8 = 9;
const VALUE_LAMBDA: u8 = 10;
const VALUE_MACRO: u8 = 11;
const VALUE_SUBR: u8 = 12;
const VALUE_BYTE_CODE: u8 = 13;
const VALUE_MARKER: u8 = 14;
const VALUE_OVERLAY: u8 = 15;
const VALUE_BUFFER: u8 = 16;
const VALUE_WINDOW: u8 = 17;
const VALUE_FRAME: u8 = 18;
const VALUE_TIMER: u8 = 19;
const VALUE_BIGNUM: u8 = 20;
const VALUE_UNBOUND: u8 = 21;

pub(crate) fn write_value(out: &mut Vec<u8>, value: &DumpValue) -> Result<(), DumpError> {
    match value {
        DumpValue::Nil => write_u8(out, VALUE_NIL),
        DumpValue::True => write_u8(out, VALUE_TRUE),
        DumpValue::Int(n) => {
            write_u8(out, VALUE_INT);
            write_i64(out, *n);
        }
        DumpValue::Float(id) => write_heap_ref_value(out, VALUE_FLOAT, id),
        DumpValue::Symbol(id) => {
            write_u8(out, VALUE_SYMBOL);
            write_u32(out, id.0);
        }
        DumpValue::Str(id) => write_heap_ref_value(out, VALUE_STR, id),
        DumpValue::Cons(id) => write_heap_ref_value(out, VALUE_CONS, id),
        DumpValue::Vector(id) => write_heap_ref_value(out, VALUE_VECTOR, id),
        DumpValue::Record(id) => write_heap_ref_value(out, VALUE_RECORD, id),
        DumpValue::HashTable(id) => write_heap_ref_value(out, VALUE_HASH_TABLE, id),
        DumpValue::Lambda(id) => write_heap_ref_value(out, VALUE_LAMBDA, id),
        DumpValue::Macro(id) => write_heap_ref_value(out, VALUE_MACRO, id),
        DumpValue::Subr(id) => {
            write_u8(out, VALUE_SUBR);
            write_u32(out, id.0);
        }
        DumpValue::ByteCode(id) => write_heap_ref_value(out, VALUE_BYTE_CODE, id),
        DumpValue::Marker(id) => write_heap_ref_value(out, VALUE_MARKER, id),
        DumpValue::Overlay(id) => write_heap_ref_value(out, VALUE_OVERLAY, id),
        DumpValue::Buffer(id) => {
            write_u8(out, VALUE_BUFFER);
            write_u64(out, id.0);
        }
        DumpValue::Window(id) => {
            write_u8(out, VALUE_WINDOW);
            write_u64(out, *id);
        }
        DumpValue::Frame(id) => {
            write_u8(out, VALUE_FRAME);
            write_u64(out, *id);
        }
        DumpValue::Timer(id) => {
            write_u8(out, VALUE_TIMER);
            write_u64(out, *id);
        }
        DumpValue::Bignum(text) => {
            write_u8(out, VALUE_BIGNUM);
            write_string(out, text)?;
        }
        DumpValue::Unbound => write_u8(out, VALUE_UNBOUND),
    }
    Ok(())
}

fn write_heap_ref_value(out: &mut Vec<u8>, tag: u8, id: &DumpHeapRef) {
    write_u8(out, tag);
    write_u32(out, id.index);
}

fn write_values(out: &mut Vec<u8>, values: &[DumpValue]) -> Result<(), DumpError> {
    write_len(out, values.len(), "value count")?;
    for value in values {
        write_value(out, value)?;
    }
    Ok(())
}

fn write_hash_table(out: &mut Vec<u8>, table: &DumpLispHashTable) -> Result<(), DumpError> {
    write_hash_table_test(out, &table.test);
    write_opt_sym_id(out, table.test_name);
    write_i64(out, table.size);
    write_opt_hash_table_weakness(out, table.weakness.as_ref());
    write_f64(out, table.rehash_size);
    write_f64(out, table.rehash_threshold);
    write_hash_entries(out, &table.entries)?;
    write_hash_entries(out, &table.key_snapshots)?;
    write_hash_keys(out, &table.insertion_order)?;
    Ok(())
}

const HASH_TEST_EQ: u8 = 0;
const HASH_TEST_EQL: u8 = 1;
const HASH_TEST_EQUAL: u8 = 2;

fn write_hash_table_test(out: &mut Vec<u8>, test: &DumpHashTableTest) {
    write_u8(
        out,
        match test {
            DumpHashTableTest::Eq => HASH_TEST_EQ,
            DumpHashTableTest::Eql => HASH_TEST_EQL,
            DumpHashTableTest::Equal => HASH_TEST_EQUAL,
        },
    );
}

const HASH_WEAKNESS_KEY: u8 = 0;
const HASH_WEAKNESS_VALUE: u8 = 1;
const HASH_WEAKNESS_KEY_OR_VALUE: u8 = 2;
const HASH_WEAKNESS_KEY_AND_VALUE: u8 = 3;

fn write_opt_hash_table_weakness(out: &mut Vec<u8>, weakness: Option<&DumpHashTableWeakness>) {
    match weakness {
        Some(weakness) => {
            write_bool(out, true);
            write_u8(
                out,
                match weakness {
                    DumpHashTableWeakness::Key => HASH_WEAKNESS_KEY,
                    DumpHashTableWeakness::Value => HASH_WEAKNESS_VALUE,
                    DumpHashTableWeakness::KeyOrValue => HASH_WEAKNESS_KEY_OR_VALUE,
                    DumpHashTableWeakness::KeyAndValue => HASH_WEAKNESS_KEY_AND_VALUE,
                },
            );
        }
        None => write_bool(out, false),
    }
}

const HASH_KEY_NIL: u8 = 0;
const HASH_KEY_TRUE: u8 = 1;
const HASH_KEY_INT: u8 = 2;
const HASH_KEY_FLOAT: u8 = 3;
const HASH_KEY_FLOAT_EQ: u8 = 4;
const HASH_KEY_SYMBOL: u8 = 5;
const HASH_KEY_KEYWORD: u8 = 6;
const HASH_KEY_STR: u8 = 7;
const HASH_KEY_CHAR: u8 = 8;
const HASH_KEY_WINDOW: u8 = 9;
const HASH_KEY_FRAME: u8 = 10;
const HASH_KEY_PTR: u8 = 11;
const HASH_KEY_HEAP_REF: u8 = 12;
const HASH_KEY_EQUAL_CONS: u8 = 13;
const HASH_KEY_EQUAL_VEC: u8 = 14;
const HASH_KEY_SYMBOL_WITH_POS: u8 = 15;
const HASH_KEY_CYCLE: u8 = 16;
const HASH_KEY_TEXT: u8 = 17;

fn write_hash_key(out: &mut Vec<u8>, key: &DumpHashKey) -> Result<(), DumpError> {
    match key {
        DumpHashKey::Nil => write_u8(out, HASH_KEY_NIL),
        DumpHashKey::True => write_u8(out, HASH_KEY_TRUE),
        DumpHashKey::Int(value) => {
            write_u8(out, HASH_KEY_INT);
            write_i64(out, *value);
        }
        DumpHashKey::Float(value) => {
            write_u8(out, HASH_KEY_FLOAT);
            write_u64(out, *value);
        }
        DumpHashKey::FloatEq(value, eq_hash) => {
            write_u8(out, HASH_KEY_FLOAT_EQ);
            write_u64(out, *value);
            write_u32(out, *eq_hash);
        }
        DumpHashKey::Symbol(id) => {
            write_u8(out, HASH_KEY_SYMBOL);
            write_u32(out, id.0);
        }
        DumpHashKey::Keyword(id) => {
            write_u8(out, HASH_KEY_KEYWORD);
            write_u32(out, id.0);
        }
        DumpHashKey::Str(id) => {
            write_u8(out, HASH_KEY_STR);
            write_u32(out, id.index);
        }
        DumpHashKey::Char(ch) => {
            write_u8(out, HASH_KEY_CHAR);
            write_u32(out, *ch as u32);
        }
        DumpHashKey::Window(id) => {
            write_u8(out, HASH_KEY_WINDOW);
            write_u64(out, *id);
        }
        DumpHashKey::Frame(id) => {
            write_u8(out, HASH_KEY_FRAME);
            write_u64(out, *id);
        }
        DumpHashKey::Ptr(id) => {
            write_u8(out, HASH_KEY_PTR);
            write_u64(out, *id);
        }
        DumpHashKey::HeapRef(index) => {
            write_u8(out, HASH_KEY_HEAP_REF);
            write_u32(out, *index);
        }
        DumpHashKey::EqualCons(car, cdr) => {
            write_u8(out, HASH_KEY_EQUAL_CONS);
            write_hash_key(out, car)?;
            write_hash_key(out, cdr)?;
        }
        DumpHashKey::EqualVec(keys) => {
            write_u8(out, HASH_KEY_EQUAL_VEC);
            write_hash_keys(out, keys)?;
        }
        DumpHashKey::SymbolWithPos(symbol, pos) => {
            write_u8(out, HASH_KEY_SYMBOL_WITH_POS);
            write_hash_key(out, symbol)?;
            write_hash_key(out, pos)?;
        }
        DumpHashKey::Cycle(index) => {
            write_u8(out, HASH_KEY_CYCLE);
            write_u32(out, *index);
        }
        DumpHashKey::Text(text) => {
            write_u8(out, HASH_KEY_TEXT);
            write_string(out, text)?;
        }
    }
    Ok(())
}

fn write_hash_keys(out: &mut Vec<u8>, keys: &[DumpHashKey]) -> Result<(), DumpError> {
    write_len(out, keys.len(), "hash key count")?;
    for key in keys {
        write_hash_key(out, key)?;
    }
    Ok(())
}

fn write_hash_entries(
    out: &mut Vec<u8>,
    entries: &[(DumpHashKey, DumpValue)],
) -> Result<(), DumpError> {
    write_len(out, entries.len(), "hash entry count")?;
    for (key, value) in entries {
        write_hash_key(out, key)?;
        write_value(out, value)?;
    }
    Ok(())
}

fn write_text_property_runs(
    out: &mut Vec<u8>,
    runs: &[DumpStringTextPropertyRun],
) -> Result<(), DumpError> {
    write_len(out, runs.len(), "string text property run count")?;
    for run in runs {
        write_usize(out, run.start)?;
        write_usize(out, run.end)?;
        write_value(out, &run.plist)?;
    }
    Ok(())
}

fn write_byte_code(out: &mut Vec<u8>, function: &DumpByteCodeFunction) -> Result<(), DumpError> {
    write_ops(out, &function.ops);
    write_values(out, &function.constants)?;
    write_u16(out, function.max_stack);
    write_lambda_params(out, &function.params)?;
    write_opt_value(out, function.arglist.as_ref())?;
    write_bool(out, function.lexical);
    write_opt_value(out, function.env.as_ref())?;
    write_opt_u32_pairs(out, function.gnu_byte_offset_map.as_ref())?;
    write_opt_lisp_string(out, function.docstring.as_ref())?;
    write_opt_value(out, function.doc_form.as_ref())?;
    write_opt_value(out, function.interactive.as_ref())?;
    Ok(())
}

fn write_lambda_params(out: &mut Vec<u8>, params: &DumpLambdaParams) -> Result<(), DumpError> {
    write_sym_ids(out, &params.required)?;
    write_sym_ids(out, &params.optional)?;
    write_opt_sym_id(out, params.rest);
    Ok(())
}

fn write_sym_ids(out: &mut Vec<u8>, syms: &[DumpSymId]) -> Result<(), DumpError> {
    write_len(out, syms.len(), "symbol id count")?;
    for sym in syms {
        write_u32(out, sym.0);
    }
    Ok(())
}

const OP_CONSTANT: u8 = 0;
const OP_NIL: u8 = 1;
const OP_TRUE: u8 = 2;
const OP_POP: u8 = 3;
const OP_DUP: u8 = 4;
const OP_STACK_REF: u8 = 5;
const OP_STACK_SET: u8 = 6;
const OP_DISCARD_N: u8 = 7;
const OP_VAR_REF: u8 = 8;
const OP_VAR_SET: u8 = 9;
const OP_VAR_BIND: u8 = 10;
const OP_UNBIND: u8 = 11;
const OP_CALL: u8 = 12;
const OP_APPLY: u8 = 13;
const OP_GOTO: u8 = 14;
const OP_GOTO_IF_NIL: u8 = 15;
const OP_GOTO_IF_NOT_NIL: u8 = 16;
const OP_GOTO_IF_NIL_ELSE_POP: u8 = 17;
const OP_GOTO_IF_NOT_NIL_ELSE_POP: u8 = 18;
const OP_SWITCH: u8 = 19;
const OP_RETURN: u8 = 20;
const OP_ADD: u8 = 21;
const OP_SUB: u8 = 22;
const OP_MUL: u8 = 23;
const OP_DIV: u8 = 24;
const OP_REM: u8 = 25;
const OP_ADD1: u8 = 26;
const OP_SUB1: u8 = 27;
const OP_NEGATE: u8 = 28;
const OP_EQLSIGN: u8 = 29;
const OP_GTR: u8 = 30;
const OP_LSS: u8 = 31;
const OP_LEQ: u8 = 32;
const OP_GEQ: u8 = 33;
const OP_MAX: u8 = 34;
const OP_MIN: u8 = 35;
const OP_CAR: u8 = 36;
const OP_CDR: u8 = 37;
const OP_CONS: u8 = 38;
const OP_LIST: u8 = 39;
const OP_LENGTH: u8 = 40;
const OP_NTH: u8 = 41;
const OP_NTHCDR: u8 = 42;
const OP_SETCAR: u8 = 43;
const OP_SETCDR: u8 = 44;
const OP_CAR_SAFE: u8 = 45;
const OP_CDR_SAFE: u8 = 46;
const OP_ELT: u8 = 47;
const OP_NCONC: u8 = 48;
const OP_NREVERSE: u8 = 49;
const OP_MEMBER: u8 = 50;
const OP_MEMQ: u8 = 51;
const OP_ASSQ: u8 = 52;
const OP_SYMBOLP: u8 = 53;
const OP_CONSP: u8 = 54;
const OP_STRINGP: u8 = 55;
const OP_LISTP: u8 = 56;
const OP_INTEGERP: u8 = 57;
const OP_NUMBERP: u8 = 58;
const OP_NULL: u8 = 59;
const OP_NOT: u8 = 60;
const OP_EQ: u8 = 61;
const OP_EQUAL: u8 = 62;
const OP_CONCAT: u8 = 63;
const OP_SUBSTRING: u8 = 64;
const OP_STRING_EQUAL: u8 = 65;
const OP_STRING_LESSP: u8 = 66;
const OP_AREF: u8 = 67;
const OP_ASET: u8 = 68;
const OP_SYMBOL_VALUE: u8 = 69;
const OP_SYMBOL_FUNCTION: u8 = 70;
const OP_SET: u8 = 71;
const OP_FSET: u8 = 72;
const OP_GET: u8 = 73;
const OP_PUT: u8 = 74;
const OP_PUSH_CONDITION_CASE: u8 = 75;
const OP_PUSH_CONDITION_CASE_RAW: u8 = 76;
const OP_PUSH_CATCH: u8 = 77;
const OP_POP_HANDLER: u8 = 78;
const OP_UNWIND_PROTECT: u8 = 79;
const OP_UNWIND_PROTECT_POP: u8 = 80;
const OP_THROW: u8 = 81;
const OP_SAVE_CURRENT_BUFFER: u8 = 82;
const OP_SAVE_EXCURSION: u8 = 83;
const OP_SAVE_RESTRICTION: u8 = 84;
const OP_SAVE_WINDOW_EXCURSION: u8 = 85;
const OP_MAKE_CLOSURE: u8 = 86;
const OP_CALL_BUILTIN: u8 = 87;
const OP_CALL_BUILTIN_SYM: u8 = 88;

fn write_ops(out: &mut Vec<u8>, ops: &[DumpOp]) {
    write_u64(out, ops.len() as u64);
    for op in ops {
        match op {
            DumpOp::Constant(value) => write_op_u16(out, OP_CONSTANT, *value),
            DumpOp::Nil => write_u8(out, OP_NIL),
            DumpOp::True => write_u8(out, OP_TRUE),
            DumpOp::Pop => write_u8(out, OP_POP),
            DumpOp::Dup => write_u8(out, OP_DUP),
            DumpOp::StackRef(value) => write_op_u16(out, OP_STACK_REF, *value),
            DumpOp::StackSet(value) => write_op_u16(out, OP_STACK_SET, *value),
            DumpOp::DiscardN(value) => {
                write_u8(out, OP_DISCARD_N);
                write_u8(out, *value);
            }
            DumpOp::VarRef(value) => write_op_u16(out, OP_VAR_REF, *value),
            DumpOp::VarSet(value) => write_op_u16(out, OP_VAR_SET, *value),
            DumpOp::VarBind(value) => write_op_u16(out, OP_VAR_BIND, *value),
            DumpOp::Unbind(value) => write_op_u16(out, OP_UNBIND, *value),
            DumpOp::Call(value) => write_op_u16(out, OP_CALL, *value),
            DumpOp::Apply(value) => write_op_u16(out, OP_APPLY, *value),
            DumpOp::Goto(value) => write_op_u32(out, OP_GOTO, *value),
            DumpOp::GotoIfNil(value) => write_op_u32(out, OP_GOTO_IF_NIL, *value),
            DumpOp::GotoIfNotNil(value) => write_op_u32(out, OP_GOTO_IF_NOT_NIL, *value),
            DumpOp::GotoIfNilElsePop(value) => write_op_u32(out, OP_GOTO_IF_NIL_ELSE_POP, *value),
            DumpOp::GotoIfNotNilElsePop(value) => {
                write_op_u32(out, OP_GOTO_IF_NOT_NIL_ELSE_POP, *value);
            }
            DumpOp::Switch => write_u8(out, OP_SWITCH),
            DumpOp::Return => write_u8(out, OP_RETURN),
            DumpOp::Add => write_u8(out, OP_ADD),
            DumpOp::Sub => write_u8(out, OP_SUB),
            DumpOp::Mul => write_u8(out, OP_MUL),
            DumpOp::Div => write_u8(out, OP_DIV),
            DumpOp::Rem => write_u8(out, OP_REM),
            DumpOp::Add1 => write_u8(out, OP_ADD1),
            DumpOp::Sub1 => write_u8(out, OP_SUB1),
            DumpOp::Negate => write_u8(out, OP_NEGATE),
            DumpOp::Eqlsign => write_u8(out, OP_EQLSIGN),
            DumpOp::Gtr => write_u8(out, OP_GTR),
            DumpOp::Lss => write_u8(out, OP_LSS),
            DumpOp::Leq => write_u8(out, OP_LEQ),
            DumpOp::Geq => write_u8(out, OP_GEQ),
            DumpOp::Max => write_u8(out, OP_MAX),
            DumpOp::Min => write_u8(out, OP_MIN),
            DumpOp::Car => write_u8(out, OP_CAR),
            DumpOp::Cdr => write_u8(out, OP_CDR),
            DumpOp::Cons => write_u8(out, OP_CONS),
            DumpOp::List(value) => write_op_u16(out, OP_LIST, *value),
            DumpOp::Length => write_u8(out, OP_LENGTH),
            DumpOp::Nth => write_u8(out, OP_NTH),
            DumpOp::Nthcdr => write_u8(out, OP_NTHCDR),
            DumpOp::Setcar => write_u8(out, OP_SETCAR),
            DumpOp::Setcdr => write_u8(out, OP_SETCDR),
            DumpOp::CarSafe => write_u8(out, OP_CAR_SAFE),
            DumpOp::CdrSafe => write_u8(out, OP_CDR_SAFE),
            DumpOp::Elt => write_u8(out, OP_ELT),
            DumpOp::Nconc => write_u8(out, OP_NCONC),
            DumpOp::Nreverse => write_u8(out, OP_NREVERSE),
            DumpOp::Member => write_u8(out, OP_MEMBER),
            DumpOp::Memq => write_u8(out, OP_MEMQ),
            DumpOp::Assq => write_u8(out, OP_ASSQ),
            DumpOp::Symbolp => write_u8(out, OP_SYMBOLP),
            DumpOp::Consp => write_u8(out, OP_CONSP),
            DumpOp::Stringp => write_u8(out, OP_STRINGP),
            DumpOp::Listp => write_u8(out, OP_LISTP),
            DumpOp::Integerp => write_u8(out, OP_INTEGERP),
            DumpOp::Numberp => write_u8(out, OP_NUMBERP),
            DumpOp::Null => write_u8(out, OP_NULL),
            DumpOp::Not => write_u8(out, OP_NOT),
            DumpOp::Eq => write_u8(out, OP_EQ),
            DumpOp::Equal => write_u8(out, OP_EQUAL),
            DumpOp::Concat(value) => write_op_u16(out, OP_CONCAT, *value),
            DumpOp::Substring => write_u8(out, OP_SUBSTRING),
            DumpOp::StringEqual => write_u8(out, OP_STRING_EQUAL),
            DumpOp::StringLessp => write_u8(out, OP_STRING_LESSP),
            DumpOp::Aref => write_u8(out, OP_AREF),
            DumpOp::Aset => write_u8(out, OP_ASET),
            DumpOp::SymbolValue => write_u8(out, OP_SYMBOL_VALUE),
            DumpOp::SymbolFunction => write_u8(out, OP_SYMBOL_FUNCTION),
            DumpOp::Set => write_u8(out, OP_SET),
            DumpOp::Fset => write_u8(out, OP_FSET),
            DumpOp::Get => write_u8(out, OP_GET),
            DumpOp::Put => write_u8(out, OP_PUT),
            DumpOp::PushConditionCase(value) => write_op_u32(out, OP_PUSH_CONDITION_CASE, *value),
            DumpOp::PushConditionCaseRaw(value) => {
                write_op_u32(out, OP_PUSH_CONDITION_CASE_RAW, *value);
            }
            DumpOp::PushCatch(value) => write_op_u32(out, OP_PUSH_CATCH, *value),
            DumpOp::PopHandler => write_u8(out, OP_POP_HANDLER),
            DumpOp::UnwindProtect(value) => write_op_u32(out, OP_UNWIND_PROTECT, *value),
            DumpOp::UnwindProtectPop => write_u8(out, OP_UNWIND_PROTECT_POP),
            DumpOp::Throw => write_u8(out, OP_THROW),
            DumpOp::SaveCurrentBuffer => write_u8(out, OP_SAVE_CURRENT_BUFFER),
            DumpOp::SaveExcursion => write_u8(out, OP_SAVE_EXCURSION),
            DumpOp::SaveRestriction => write_u8(out, OP_SAVE_RESTRICTION),
            DumpOp::SaveWindowExcursion => write_u8(out, OP_SAVE_WINDOW_EXCURSION),
            DumpOp::MakeClosure(value) => write_op_u16(out, OP_MAKE_CLOSURE, *value),
            DumpOp::CallBuiltin(index, argc) => {
                write_u8(out, OP_CALL_BUILTIN);
                write_u16(out, *index);
                write_u8(out, *argc);
            }
            DumpOp::CallBuiltinSym(sym, argc) => {
                write_u8(out, OP_CALL_BUILTIN_SYM);
                write_u32(out, sym.0);
                write_u8(out, *argc);
            }
        }
    }
}

fn write_op_u16(out: &mut Vec<u8>, tag: u8, value: u16) {
    write_u8(out, tag);
    write_u16(out, value);
}

fn write_op_u32(out: &mut Vec<u8>, tag: u8, value: u32) {
    write_u8(out, tag);
    write_u32(out, value);
}

fn write_marker(out: &mut Vec<u8>, marker: &DumpMarker) -> Result<(), DumpError> {
    write_opt_buffer_id(out, marker.buffer);
    write_bool(out, marker.insertion_type);
    write_opt_u64(out, marker.marker_id);
    write_usize(out, marker.bytepos)?;
    write_usize(out, marker.charpos)?;
    Ok(())
}

fn write_overlay(out: &mut Vec<u8>, overlay: &DumpOverlay) -> Result<(), DumpError> {
    write_value(out, &overlay.plist)?;
    write_opt_buffer_id(out, overlay.buffer);
    write_usize(out, overlay.start)?;
    write_usize(out, overlay.end)?;
    write_bool(out, overlay.front_advance);
    write_bool(out, overlay.rear_advance);
    Ok(())
}

fn write_lisp_string(out: &mut Vec<u8>, string: &DumpLispString) -> Result<(), DumpError> {
    write_bytes(out, &string.data)?;
    write_usize(out, string.size)?;
    write_i64(out, string.size_byte);
    Ok(())
}

fn write_opt_lisp_string(
    out: &mut Vec<u8>,
    string: Option<&DumpLispString>,
) -> Result<(), DumpError> {
    match string {
        Some(string) => {
            write_bool(out, true);
            write_lisp_string(out, string)?;
        }
        None => write_bool(out, false),
    }
    Ok(())
}

fn write_opt_value(out: &mut Vec<u8>, value: Option<&DumpValue>) -> Result<(), DumpError> {
    match value {
        Some(value) => {
            write_bool(out, true);
            write_value(out, value)?;
        }
        None => write_bool(out, false),
    }
    Ok(())
}

fn write_opt_sym_id(out: &mut Vec<u8>, id: Option<DumpSymId>) {
    match id {
        Some(id) => {
            write_bool(out, true);
            write_u32(out, id.0);
        }
        None => write_bool(out, false),
    }
}

fn write_opt_buffer_id(out: &mut Vec<u8>, id: Option<DumpBufferId>) {
    match id {
        Some(id) => {
            write_bool(out, true);
            write_u64(out, id.0);
        }
        None => write_bool(out, false),
    }
}

fn write_opt_u16(out: &mut Vec<u8>, value: Option<u16>) {
    match value {
        Some(value) => {
            write_bool(out, true);
            write_u16(out, value);
        }
        None => write_bool(out, false),
    }
}

fn write_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            write_bool(out, true);
            write_u64(out, value);
        }
        None => write_bool(out, false),
    }
}

fn write_opt_u32_pairs(
    out: &mut Vec<u8>,
    pairs: Option<&Vec<(u32, u32)>>,
) -> Result<(), DumpError> {
    match pairs {
        Some(pairs) => {
            write_bool(out, true);
            write_len(out, pairs.len(), "u32 pair count")?;
            for (left, right) in pairs {
                write_u32(out, *left);
                write_u32(out, *right);
            }
        }
        None => write_bool(out, false),
    }
    Ok(())
}

fn write_len(out: &mut Vec<u8>, len: usize, what: &str) -> Result<(), DumpError> {
    let len = u64::try_from(len)
        .map_err(|_| DumpError::SerializationError(format!("{what} overflows u64")))?;
    write_u64(out, len);
    Ok(())
}

fn write_usize(out: &mut Vec<u8>, value: usize) -> Result<(), DumpError> {
    let value = u64::try_from(value)
        .map_err(|_| DumpError::SerializationError("usize value overflows u64".into()))?;
    write_u64(out, value);
    Ok(())
}

fn write_string(out: &mut Vec<u8>, text: &str) -> Result<(), DumpError> {
    write_bytes(out, text.as_bytes())
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), DumpError> {
    write_len(out, bytes.len(), "byte payload length")?;
    out.extend_from_slice(bytes);
    Ok(())
}

pub(crate) fn write_bool(out: &mut Vec<u8>, value: bool) {
    write_u8(out, u8::from(value));
}

pub(crate) fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_ne_bytes());
}

pub(crate) fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_ne_bytes());
}

pub(crate) fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_ne_bytes());
}

fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_ne_bytes());
}

fn write_f64(out: &mut Vec<u8>, value: f64) {
    out.extend_from_slice(&value.to_ne_bytes());
}

pub(crate) struct Cursor<'a> {
    section: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(section: &'a [u8]) -> Self {
        Self { section, offset: 0 }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.offset == self.section.len()
    }

    pub(crate) fn remaining(&self) -> usize {
        self.section.len() - self.offset
    }

    fn read_heap_object(&mut self, index: usize) -> Result<DumpHeapObject, DumpError> {
        let tag = self.read_u8("heap object tag")?;
        match tag {
            HEAP_CONS => Ok(DumpHeapObject::Cons {
                car: self.read_value()?,
                cdr: self.read_value()?,
            }),
            HEAP_VECTOR => Ok(DumpHeapObject::Vector(self.read_values()?)),
            HEAP_HASH_TABLE => Ok(DumpHeapObject::HashTable(self.read_hash_table()?)),
            HEAP_STRING => Ok(DumpHeapObject::Str {
                data: self.read_byte_data()?,
                size: self.read_usize("string char size")?,
                size_byte: self.read_i64("string byte size")?,
                text_props: self.read_text_property_runs()?,
            }),
            HEAP_FLOAT => Ok(DumpHeapObject::Float(self.read_f64("float object")?)),
            HEAP_LAMBDA => Ok(DumpHeapObject::Lambda(self.read_values()?)),
            HEAP_MACRO => Ok(DumpHeapObject::Macro(self.read_values()?)),
            HEAP_BYTE_CODE => Ok(DumpHeapObject::ByteCode(self.read_byte_code()?)),
            HEAP_RECORD => Ok(DumpHeapObject::Record(self.read_values()?)),
            HEAP_MARKER => Ok(DumpHeapObject::Marker(self.read_marker()?)),
            HEAP_OVERLAY => Ok(DumpHeapObject::Overlay(self.read_overlay()?)),
            HEAP_BUFFER => Ok(DumpHeapObject::Buffer(DumpBufferId(
                self.read_u64("buffer object id")?,
            ))),
            HEAP_WINDOW => Ok(DumpHeapObject::Window(self.read_u64("window object id")?)),
            HEAP_FRAME => Ok(DumpHeapObject::Frame(self.read_u64("frame object id")?)),
            HEAP_TIMER => Ok(DumpHeapObject::Timer(self.read_u64("timer object id")?)),
            HEAP_SUBR => Ok(DumpHeapObject::Subr {
                name: DumpNameId(self.read_u32("subr name id")?),
                min_args: self.read_u16("subr min args")?,
                max_args: self.read_opt_u16()?,
            }),
            HEAP_FREE => Ok(DumpHeapObject::Free),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown heap object tag {other} at object {index}"
            ))),
        }
    }

    fn read_byte_data(&mut self) -> Result<DumpByteData, DumpError> {
        match self.read_u8("byte data tag")? {
            BYTE_OWNED => Ok(DumpByteData::owned(self.read_bytes()?)),
            BYTE_MAPPED => Ok(DumpByteData::mapped(
                self.read_u64("mapped byte offset")?,
                self.read_u64("mapped byte length")?,
            )),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown byte data tag {other}"
            ))),
        }
    }

    pub(crate) fn read_value(&mut self) -> Result<DumpValue, DumpError> {
        match self.read_u8("dump value tag")? {
            VALUE_NIL => Ok(DumpValue::Nil),
            VALUE_TRUE => Ok(DumpValue::True),
            VALUE_INT => Ok(DumpValue::Int(self.read_i64("fixnum value")?)),
            VALUE_FLOAT => Ok(DumpValue::Float(self.read_heap_ref("float heap ref")?)),
            VALUE_SYMBOL => Ok(DumpValue::Symbol(DumpSymId(self.read_u32("symbol id")?))),
            VALUE_STR => Ok(DumpValue::Str(self.read_heap_ref("string heap ref")?)),
            VALUE_CONS => Ok(DumpValue::Cons(self.read_heap_ref("cons heap ref")?)),
            VALUE_VECTOR => Ok(DumpValue::Vector(self.read_heap_ref("vector heap ref")?)),
            VALUE_RECORD => Ok(DumpValue::Record(self.read_heap_ref("record heap ref")?)),
            VALUE_HASH_TABLE => Ok(DumpValue::HashTable(
                self.read_heap_ref("hash table heap ref")?,
            )),
            VALUE_LAMBDA => Ok(DumpValue::Lambda(self.read_heap_ref("lambda heap ref")?)),
            VALUE_MACRO => Ok(DumpValue::Macro(self.read_heap_ref("macro heap ref")?)),
            VALUE_SUBR => Ok(DumpValue::Subr(DumpNameId(self.read_u32("subr id")?))),
            VALUE_BYTE_CODE => Ok(DumpValue::ByteCode(
                self.read_heap_ref("bytecode heap ref")?,
            )),
            VALUE_MARKER => Ok(DumpValue::Marker(self.read_heap_ref("marker heap ref")?)),
            VALUE_OVERLAY => Ok(DumpValue::Overlay(self.read_heap_ref("overlay heap ref")?)),
            VALUE_BUFFER => Ok(DumpValue::Buffer(DumpBufferId(self.read_u64("buffer id")?))),
            VALUE_WINDOW => Ok(DumpValue::Window(self.read_u64("window id")?)),
            VALUE_FRAME => Ok(DumpValue::Frame(self.read_u64("frame id")?)),
            VALUE_TIMER => Ok(DumpValue::Timer(self.read_u64("timer id")?)),
            VALUE_BIGNUM => Ok(DumpValue::Bignum(self.read_string()?)),
            VALUE_UNBOUND => Ok(DumpValue::Unbound),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown dump value tag {other}"
            ))),
        }
    }

    fn read_heap_ref(&mut self, what: &str) -> Result<DumpHeapRef, DumpError> {
        Ok(DumpHeapRef {
            index: self.read_u32(what)?,
        })
    }

    fn read_values(&mut self) -> Result<Vec<DumpValue>, DumpError> {
        let len = self.read_len("value count")?;
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(self.read_value()?);
        }
        Ok(values)
    }

    fn read_hash_table(&mut self) -> Result<DumpLispHashTable, DumpError> {
        Ok(DumpLispHashTable {
            test: self.read_hash_table_test()?,
            test_name: self.read_opt_sym_id()?,
            size: self.read_i64("hash table size")?,
            weakness: self.read_opt_hash_table_weakness()?,
            rehash_size: self.read_f64("hash table rehash size")?,
            rehash_threshold: self.read_f64("hash table rehash threshold")?,
            entries: self.read_hash_entries()?,
            key_snapshots: self.read_hash_entries()?,
            insertion_order: self.read_hash_keys()?,
        })
    }

    fn read_hash_table_test(&mut self) -> Result<DumpHashTableTest, DumpError> {
        match self.read_u8("hash table test")? {
            HASH_TEST_EQ => Ok(DumpHashTableTest::Eq),
            HASH_TEST_EQL => Ok(DumpHashTableTest::Eql),
            HASH_TEST_EQUAL => Ok(DumpHashTableTest::Equal),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown hash table test tag {other}"
            ))),
        }
    }

    fn read_opt_hash_table_weakness(&mut self) -> Result<Option<DumpHashTableWeakness>, DumpError> {
        if !self.read_bool("hash table weakness present")? {
            return Ok(None);
        }
        match self.read_u8("hash table weakness")? {
            HASH_WEAKNESS_KEY => Ok(Some(DumpHashTableWeakness::Key)),
            HASH_WEAKNESS_VALUE => Ok(Some(DumpHashTableWeakness::Value)),
            HASH_WEAKNESS_KEY_OR_VALUE => Ok(Some(DumpHashTableWeakness::KeyOrValue)),
            HASH_WEAKNESS_KEY_AND_VALUE => Ok(Some(DumpHashTableWeakness::KeyAndValue)),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown hash table weakness tag {other}"
            ))),
        }
    }

    fn read_hash_key(&mut self) -> Result<DumpHashKey, DumpError> {
        match self.read_u8("hash key tag")? {
            HASH_KEY_NIL => Ok(DumpHashKey::Nil),
            HASH_KEY_TRUE => Ok(DumpHashKey::True),
            HASH_KEY_INT => Ok(DumpHashKey::Int(self.read_i64("hash int key")?)),
            HASH_KEY_FLOAT => Ok(DumpHashKey::Float(self.read_u64("hash float key")?)),
            HASH_KEY_FLOAT_EQ => Ok(DumpHashKey::FloatEq(
                self.read_u64("hash float eq key")?,
                self.read_u32("hash float eq hash")?,
            )),
            HASH_KEY_SYMBOL => Ok(DumpHashKey::Symbol(DumpSymId(
                self.read_u32("hash symbol key")?,
            ))),
            HASH_KEY_KEYWORD => Ok(DumpHashKey::Keyword(DumpSymId(
                self.read_u32("hash keyword key")?,
            ))),
            HASH_KEY_STR => Ok(DumpHashKey::Str(DumpHeapRef {
                index: self.read_u32("hash string key")?,
            })),
            HASH_KEY_CHAR => {
                let raw = self.read_u32("hash char key")?;
                let ch = char::from_u32(raw).ok_or_else(|| {
                    DumpError::ImageFormatError(format!("invalid hash char scalar {raw}"))
                })?;
                Ok(DumpHashKey::Char(ch))
            }
            HASH_KEY_WINDOW => Ok(DumpHashKey::Window(self.read_u64("hash window key")?)),
            HASH_KEY_FRAME => Ok(DumpHashKey::Frame(self.read_u64("hash frame key")?)),
            HASH_KEY_PTR => Ok(DumpHashKey::Ptr(self.read_u64("hash ptr key")?)),
            HASH_KEY_HEAP_REF => Ok(DumpHashKey::HeapRef(self.read_u32("hash heap ref key")?)),
            HASH_KEY_EQUAL_CONS => Ok(DumpHashKey::EqualCons(
                Box::new(self.read_hash_key()?),
                Box::new(self.read_hash_key()?),
            )),
            HASH_KEY_EQUAL_VEC => Ok(DumpHashKey::EqualVec(self.read_hash_keys()?)),
            HASH_KEY_SYMBOL_WITH_POS => Ok(DumpHashKey::SymbolWithPos(
                Box::new(self.read_hash_key()?),
                Box::new(self.read_hash_key()?),
            )),
            HASH_KEY_CYCLE => Ok(DumpHashKey::Cycle(self.read_u32("hash cycle key")?)),
            HASH_KEY_TEXT => Ok(DumpHashKey::Text(self.read_string()?)),
            other => Err(DumpError::ImageFormatError(format!(
                "unknown hash key tag {other}"
            ))),
        }
    }

    fn read_hash_keys(&mut self) -> Result<Vec<DumpHashKey>, DumpError> {
        let len = self.read_len("hash key count")?;
        let mut keys = Vec::with_capacity(len);
        for _ in 0..len {
            keys.push(self.read_hash_key()?);
        }
        Ok(keys)
    }

    fn read_hash_entries(&mut self) -> Result<Vec<(DumpHashKey, DumpValue)>, DumpError> {
        let len = self.read_len("hash entry count")?;
        let mut entries = Vec::with_capacity(len);
        for _ in 0..len {
            entries.push((self.read_hash_key()?, self.read_value()?));
        }
        Ok(entries)
    }

    fn read_text_property_runs(&mut self) -> Result<Vec<DumpStringTextPropertyRun>, DumpError> {
        let len = self.read_len("string text property run count")?;
        let mut runs = Vec::with_capacity(len);
        for _ in 0..len {
            runs.push(DumpStringTextPropertyRun {
                start: self.read_usize("string text property start")?,
                end: self.read_usize("string text property end")?,
                plist: self.read_value()?,
            });
        }
        Ok(runs)
    }

    fn read_byte_code(&mut self) -> Result<DumpByteCodeFunction, DumpError> {
        Ok(DumpByteCodeFunction {
            ops: self.read_ops()?,
            constants: self.read_values()?,
            max_stack: self.read_u16("bytecode max stack")?,
            params: self.read_lambda_params()?,
            arglist: self.read_opt_value()?,
            lexical: self.read_bool("bytecode lexical flag")?,
            env: self.read_opt_value()?,
            gnu_byte_offset_map: self.read_opt_u32_pairs()?,
            docstring: self.read_opt_lisp_string()?,
            doc_form: self.read_opt_value()?,
            interactive: self.read_opt_value()?,
        })
    }

    fn read_lambda_params(&mut self) -> Result<DumpLambdaParams, DumpError> {
        Ok(DumpLambdaParams {
            required: self.read_sym_ids()?,
            optional: self.read_sym_ids()?,
            rest: self.read_opt_sym_id()?,
        })
    }

    fn read_sym_ids(&mut self) -> Result<Vec<DumpSymId>, DumpError> {
        let len = self.read_len("symbol id count")?;
        let mut syms = Vec::with_capacity(len);
        for _ in 0..len {
            syms.push(DumpSymId(self.read_u32("symbol id")?));
        }
        Ok(syms)
    }

    fn read_ops(&mut self) -> Result<Vec<DumpOp>, DumpError> {
        let len = self.read_len("bytecode op count")?;
        let mut ops = Vec::with_capacity(len);
        for _ in 0..len {
            let op = match self.read_u8("bytecode op tag")? {
                OP_CONSTANT => DumpOp::Constant(self.read_u16("constant op arg")?),
                OP_NIL => DumpOp::Nil,
                OP_TRUE => DumpOp::True,
                OP_POP => DumpOp::Pop,
                OP_DUP => DumpOp::Dup,
                OP_STACK_REF => DumpOp::StackRef(self.read_u16("stack-ref op arg")?),
                OP_STACK_SET => DumpOp::StackSet(self.read_u16("stack-set op arg")?),
                OP_DISCARD_N => DumpOp::DiscardN(self.read_u8("discard-n op arg")?),
                OP_VAR_REF => DumpOp::VarRef(self.read_u16("var-ref op arg")?),
                OP_VAR_SET => DumpOp::VarSet(self.read_u16("var-set op arg")?),
                OP_VAR_BIND => DumpOp::VarBind(self.read_u16("var-bind op arg")?),
                OP_UNBIND => DumpOp::Unbind(self.read_u16("unbind op arg")?),
                OP_CALL => DumpOp::Call(self.read_u16("call op arg")?),
                OP_APPLY => DumpOp::Apply(self.read_u16("apply op arg")?),
                OP_GOTO => DumpOp::Goto(self.read_u32("goto op arg")?),
                OP_GOTO_IF_NIL => DumpOp::GotoIfNil(self.read_u32("goto-if-nil op arg")?),
                OP_GOTO_IF_NOT_NIL => {
                    DumpOp::GotoIfNotNil(self.read_u32("goto-if-not-nil op arg")?)
                }
                OP_GOTO_IF_NIL_ELSE_POP => {
                    DumpOp::GotoIfNilElsePop(self.read_u32("goto-if-nil-else-pop op arg")?)
                }
                OP_GOTO_IF_NOT_NIL_ELSE_POP => {
                    DumpOp::GotoIfNotNilElsePop(self.read_u32("goto-if-not-nil-else-pop op arg")?)
                }
                OP_SWITCH => DumpOp::Switch,
                OP_RETURN => DumpOp::Return,
                OP_ADD => DumpOp::Add,
                OP_SUB => DumpOp::Sub,
                OP_MUL => DumpOp::Mul,
                OP_DIV => DumpOp::Div,
                OP_REM => DumpOp::Rem,
                OP_ADD1 => DumpOp::Add1,
                OP_SUB1 => DumpOp::Sub1,
                OP_NEGATE => DumpOp::Negate,
                OP_EQLSIGN => DumpOp::Eqlsign,
                OP_GTR => DumpOp::Gtr,
                OP_LSS => DumpOp::Lss,
                OP_LEQ => DumpOp::Leq,
                OP_GEQ => DumpOp::Geq,
                OP_MAX => DumpOp::Max,
                OP_MIN => DumpOp::Min,
                OP_CAR => DumpOp::Car,
                OP_CDR => DumpOp::Cdr,
                OP_CONS => DumpOp::Cons,
                OP_LIST => DumpOp::List(self.read_u16("list op arg")?),
                OP_LENGTH => DumpOp::Length,
                OP_NTH => DumpOp::Nth,
                OP_NTHCDR => DumpOp::Nthcdr,
                OP_SETCAR => DumpOp::Setcar,
                OP_SETCDR => DumpOp::Setcdr,
                OP_CAR_SAFE => DumpOp::CarSafe,
                OP_CDR_SAFE => DumpOp::CdrSafe,
                OP_ELT => DumpOp::Elt,
                OP_NCONC => DumpOp::Nconc,
                OP_NREVERSE => DumpOp::Nreverse,
                OP_MEMBER => DumpOp::Member,
                OP_MEMQ => DumpOp::Memq,
                OP_ASSQ => DumpOp::Assq,
                OP_SYMBOLP => DumpOp::Symbolp,
                OP_CONSP => DumpOp::Consp,
                OP_STRINGP => DumpOp::Stringp,
                OP_LISTP => DumpOp::Listp,
                OP_INTEGERP => DumpOp::Integerp,
                OP_NUMBERP => DumpOp::Numberp,
                OP_NULL => DumpOp::Null,
                OP_NOT => DumpOp::Not,
                OP_EQ => DumpOp::Eq,
                OP_EQUAL => DumpOp::Equal,
                OP_CONCAT => DumpOp::Concat(self.read_u16("concat op arg")?),
                OP_SUBSTRING => DumpOp::Substring,
                OP_STRING_EQUAL => DumpOp::StringEqual,
                OP_STRING_LESSP => DumpOp::StringLessp,
                OP_AREF => DumpOp::Aref,
                OP_ASET => DumpOp::Aset,
                OP_SYMBOL_VALUE => DumpOp::SymbolValue,
                OP_SYMBOL_FUNCTION => DumpOp::SymbolFunction,
                OP_SET => DumpOp::Set,
                OP_FSET => DumpOp::Fset,
                OP_GET => DumpOp::Get,
                OP_PUT => DumpOp::Put,
                OP_PUSH_CONDITION_CASE => {
                    DumpOp::PushConditionCase(self.read_u32("push-condition-case op arg")?)
                }
                OP_PUSH_CONDITION_CASE_RAW => {
                    DumpOp::PushConditionCaseRaw(self.read_u32("push-condition-case-raw op arg")?)
                }
                OP_PUSH_CATCH => DumpOp::PushCatch(self.read_u32("push-catch op arg")?),
                OP_POP_HANDLER => DumpOp::PopHandler,
                OP_UNWIND_PROTECT => DumpOp::UnwindProtect(self.read_u32("unwind-protect op arg")?),
                OP_UNWIND_PROTECT_POP => DumpOp::UnwindProtectPop,
                OP_THROW => DumpOp::Throw,
                OP_SAVE_CURRENT_BUFFER => DumpOp::SaveCurrentBuffer,
                OP_SAVE_EXCURSION => DumpOp::SaveExcursion,
                OP_SAVE_RESTRICTION => DumpOp::SaveRestriction,
                OP_SAVE_WINDOW_EXCURSION => DumpOp::SaveWindowExcursion,
                OP_MAKE_CLOSURE => DumpOp::MakeClosure(self.read_u16("make-closure op arg")?),
                OP_CALL_BUILTIN => DumpOp::CallBuiltin(
                    self.read_u16("call-builtin op index")?,
                    self.read_u8("call-builtin op argc")?,
                ),
                OP_CALL_BUILTIN_SYM => DumpOp::CallBuiltinSym(
                    DumpSymId(self.read_u32("call-builtin-sym op symbol")?),
                    self.read_u8("call-builtin-sym op argc")?,
                ),
                other => {
                    return Err(DumpError::ImageFormatError(format!(
                        "unknown bytecode op tag {other}"
                    )));
                }
            };
            ops.push(op);
        }
        Ok(ops)
    }

    fn read_marker(&mut self) -> Result<DumpMarker, DumpError> {
        Ok(DumpMarker {
            buffer: self.read_opt_buffer_id()?,
            insertion_type: self.read_bool("marker insertion type")?,
            marker_id: self.read_opt_u64()?,
            bytepos: self.read_usize("marker byte position")?,
            charpos: self.read_usize("marker char position")?,
        })
    }

    fn read_overlay(&mut self) -> Result<DumpOverlay, DumpError> {
        Ok(DumpOverlay {
            plist: self.read_value()?,
            buffer: self.read_opt_buffer_id()?,
            start: self.read_usize("overlay start")?,
            end: self.read_usize("overlay end")?,
            front_advance: self.read_bool("overlay front advance")?,
            rear_advance: self.read_bool("overlay rear advance")?,
        })
    }

    fn read_lisp_string(&mut self) -> Result<DumpLispString, DumpError> {
        Ok(DumpLispString {
            data: self.read_bytes()?,
            size: self.read_usize("lisp string char size")?,
            size_byte: self.read_i64("lisp string byte size")?,
        })
    }

    fn read_opt_lisp_string(&mut self) -> Result<Option<DumpLispString>, DumpError> {
        if self.read_bool("lisp string present")? {
            Ok(Some(self.read_lisp_string()?))
        } else {
            Ok(None)
        }
    }

    fn read_opt_value(&mut self) -> Result<Option<DumpValue>, DumpError> {
        if self.read_bool("value present")? {
            Ok(Some(self.read_value()?))
        } else {
            Ok(None)
        }
    }

    fn read_opt_sym_id(&mut self) -> Result<Option<DumpSymId>, DumpError> {
        if self.read_bool("symbol id present")? {
            Ok(Some(DumpSymId(self.read_u32("symbol id")?)))
        } else {
            Ok(None)
        }
    }

    fn read_opt_buffer_id(&mut self) -> Result<Option<DumpBufferId>, DumpError> {
        if self.read_bool("buffer id present")? {
            Ok(Some(DumpBufferId(self.read_u64("buffer id")?)))
        } else {
            Ok(None)
        }
    }

    fn read_opt_u16(&mut self) -> Result<Option<u16>, DumpError> {
        if self.read_bool("u16 present")? {
            Ok(Some(self.read_u16("u16 option")?))
        } else {
            Ok(None)
        }
    }

    fn read_opt_u64(&mut self) -> Result<Option<u64>, DumpError> {
        if self.read_bool("u64 present")? {
            Ok(Some(self.read_u64("u64 option")?))
        } else {
            Ok(None)
        }
    }

    fn read_opt_u32_pairs(&mut self) -> Result<Option<Vec<(u32, u32)>>, DumpError> {
        if !self.read_bool("u32 pairs present")? {
            return Ok(None);
        }
        let len = self.read_len("u32 pair count")?;
        let mut pairs = Vec::with_capacity(len);
        for _ in 0..len {
            pairs.push((
                self.read_u32("u32 pair left")?,
                self.read_u32("u32 pair right")?,
            ));
        }
        Ok(Some(pairs))
    }

    pub(crate) fn read_string(&mut self) -> Result<String, DumpError> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes)
            .map_err(|e| DumpError::ImageFormatError(format!("invalid UTF-8 string: {e}")))
    }

    pub(crate) fn read_bytes(&mut self) -> Result<Vec<u8>, DumpError> {
        let len = self.read_len("byte payload length")?;
        Ok(self.read_exact(len, "byte payload")?.to_vec())
    }

    pub(crate) fn read_len(&mut self, what: &str) -> Result<usize, DumpError> {
        let len = self.read_u64(what)?;
        usize::try_from(len)
            .map_err(|_| DumpError::ImageFormatError(format!("{what} overflows usize")))
    }

    pub(crate) fn read_usize(&mut self, what: &str) -> Result<usize, DumpError> {
        let value = self.read_u64(what)?;
        usize::try_from(value)
            .map_err(|_| DumpError::ImageFormatError(format!("{what} overflows usize")))
    }

    pub(crate) fn read_bool(&mut self, what: &str) -> Result<bool, DumpError> {
        match self.read_u8(what)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DumpError::ImageFormatError(format!(
                "{what} has invalid bool byte {other}"
            ))),
        }
    }

    pub(crate) fn read_u8(&mut self, what: &str) -> Result<u8, DumpError> {
        Ok(self.read_exact(1, what)?[0])
    }

    pub(crate) fn read_u16(&mut self, what: &str) -> Result<u16, DumpError> {
        let bytes = self.read_exact(2, what)?;
        Ok(u16::from_ne_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn read_u32(&mut self, what: &str) -> Result<u32, DumpError> {
        let bytes = self.read_exact(4, what)?;
        Ok(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn read_u64(&mut self, what: &str) -> Result<u64, DumpError> {
        let bytes = self.read_exact(8, what)?;
        Ok(u64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(crate) fn read_i64(&mut self, what: &str) -> Result<i64, DumpError> {
        let bytes = self.read_exact(8, what)?;
        Ok(i64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(crate) fn read_f64(&mut self, what: &str) -> Result<f64, DumpError> {
        let bytes = self.read_exact(8, what)?;
        Ok(f64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(crate) fn read_exact(&mut self, len: usize, what: &str) -> Result<&'a [u8], DumpError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| DumpError::ImageFormatError(format!("{what} read range overflows")))?;
        if end > self.section.len() {
            return Err(DumpError::ImageFormatError(format!(
                "{what} extends past heap object section payload"
            )));
        }
        let start = self.offset;
        self.offset = end;
        Ok(&self.section[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_objects_section_round_trips_representative_objects() {
        let objects = vec![
            DumpHeapObject::Str {
                data: DumpByteData::mapped(24, 3),
                size: 3,
                size_byte: 3,
                text_props: vec![DumpStringTextPropertyRun {
                    start: 0,
                    end: 1,
                    plist: DumpValue::Symbol(DumpSymId(7)),
                }],
            },
            DumpHeapObject::Cons {
                car: DumpValue::Int(42),
                cdr: DumpValue::Str(DumpHeapRef { index: 0 }),
            },
            DumpHeapObject::ByteCode(DumpByteCodeFunction {
                ops: vec![
                    DumpOp::Constant(1),
                    DumpOp::CallBuiltinSym(DumpSymId(9), 2),
                    DumpOp::Return,
                ],
                constants: vec![DumpValue::Bignum("12345678901234567890".into())],
                max_stack: 4,
                params: DumpLambdaParams {
                    required: vec![DumpSymId(1)],
                    optional: vec![DumpSymId(2)],
                    rest: Some(DumpSymId(3)),
                },
                arglist: Some(DumpValue::Nil),
                lexical: true,
                env: Some(DumpValue::Vector(DumpHeapRef { index: 4 })),
                gnu_byte_offset_map: Some(vec![(1, 2), (3, 4)]),
                docstring: Some(DumpLispString {
                    data: b"doc".to_vec(),
                    size: 3,
                    size_byte: 3,
                }),
                doc_form: Some(DumpValue::True),
                interactive: Some(DumpValue::Nil),
            }),
            DumpHeapObject::HashTable(DumpLispHashTable {
                test: DumpHashTableTest::Equal,
                test_name: Some(DumpSymId(11)),
                size: 17,
                weakness: Some(DumpHashTableWeakness::KeyOrValue),
                rehash_size: 1.5,
                rehash_threshold: 0.8,
                entries: vec![(
                    DumpHashKey::EqualCons(
                        Box::new(DumpHashKey::Text("a".into())),
                        Box::new(DumpHashKey::Cycle(1)),
                    ),
                    DumpValue::Cons(DumpHeapRef { index: 1 }),
                )],
                key_snapshots: vec![(DumpHashKey::Char('x'), DumpValue::Int(8))],
                insertion_order: vec![DumpHashKey::HeapRef(1)],
            }),
            DumpHeapObject::Marker(DumpMarker {
                buffer: Some(DumpBufferId(5)),
                insertion_type: true,
                marker_id: Some(6),
                bytepos: 7,
                charpos: 8,
            }),
            DumpHeapObject::Overlay(DumpOverlay {
                plist: DumpValue::Nil,
                buffer: Some(DumpBufferId(9)),
                start: 10,
                end: 11,
                front_advance: true,
                rear_advance: false,
            }),
            DumpHeapObject::Subr {
                name: DumpNameId(13),
                min_args: 1,
                max_args: Some(2),
            },
        ];

        let bytes = heap_objects_section_bytes(&objects).expect("encode heap objects");
        let decoded = load_heap_objects_section(&bytes).expect("decode heap objects");

        assert_eq!(format!("{decoded:?}"), format!("{objects:?}"));
    }

    #[test]
    fn heap_objects_section_rejects_bad_magic() {
        let mut bytes = heap_objects_section_bytes(&[]).expect("encode heap objects");
        bytes[0] ^= 1;
        let err = load_heap_objects_section(&bytes).expect_err("bad magic should fail");
        assert!(matches!(err, DumpError::ImageFormatError(_)));
    }
}
