//! Compact ObjectExtra section: sparse extra data for objects not fully mapped.
//!
//! Category A objects (cons, float, vector, lambda, macro, record) are fully
//! in HeapImage/ObjectStarts after relocation and need no extra data.
//!
//! Category B objects (string, overlay, marker) have mapped HeapImage spans
//! but need a small descriptor for fields that can't be raw bytes.
//!
//! Category C objects (hash-table, bytecode, subr, buffer, window, frame,
//! timer, free) have no HeapImage representation and need a full descriptor.
//!
//! Serialization strategy: each sparse record starts with the object index, then
//! the extra tag byte identifies the variant. Complex payloads use the same
//! encoding as `object_value_codec::write_heap_object`; on read, we delegate to
//! `Cursor::read_heap_object` and extract the relevant fields from the returned
//! `DumpHeapObject`.

use bytemuck::{Pod, Zeroable};

use super::mapped_heap::MappedHeapView;
use super::object_starts::{LoadedObjectSpan, LoadedSpans};
use super::object_value_codec;
use super::{DumpError, types::*};
use crate::tagged::header::VecLikeType;

const OBJECT_EXTRA_MAGIC: [u8; 16] = *b"NEOOBJEXTRA\0\0\0\0\0";
const OBJECT_EXTRA_FORMAT_VERSION: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ObjectExtraHeader {
    magic: [u8; 16],
    version: u32,
    header_size: u32,
    object_count: u64,
    payload_offset: u64,
    payload_len: u64,
}

const HEADER_SIZE: usize = std::mem::size_of::<ObjectExtraHeader>();

// Variant tags — kept distinct from HEAP_* tags in object_value_codec.rs.
const EXTRA_STRING: u8 = 101;
const EXTRA_HASH_TABLE: u8 = 102;
const EXTRA_BYTE_CODE: u8 = 103;
const EXTRA_SUBR: u8 = 104;
const EXTRA_BUFFER: u8 = 105;
const EXTRA_WINDOW: u8 = 106;
const EXTRA_FRAME: u8 = 107;
const EXTRA_TIMER: u8 = 108;
const EXTRA_OVERLAY: u8 = 109;
const EXTRA_MARKER: u8 = 110;
const EXTRA_FREE: u8 = 111;

/// Per-object extra data needed during load.
#[derive(Debug, Clone)]
pub(crate) enum ObjectExtra {
    /// Category B: string needs size, size_byte, byte data span, and text_props.
    String {
        size: usize,
        size_byte: i64,
        byte_data: DumpByteData,
        text_props: Vec<DumpStringTextPropertyRun>,
    },
    /// Category C: hash table (no HeapImage bytes).
    HashTable(DumpLispHashTable),
    /// Category C: bytecode function (no HeapImage bytes).
    ByteCode(DumpByteCodeFunction),
    /// Category C: subr (no HeapImage bytes).
    Subr {
        name: DumpNameId,
        min_args: u16,
        max_args: Option<u16>,
    },
    /// Category C: buffer ID (no HeapImage bytes).
    Buffer(DumpBufferId),
    /// Category C: window ID (no HeapImage bytes).
    Window(u64),
    /// Category C: frame ID (no HeapImage bytes).
    Frame(u64),
    /// Category C: timer ID (no HeapImage bytes).
    Timer(u64),
    /// Category B: overlay (has veclike span but needs plist).
    Overlay(DumpOverlay),
    /// Category B: marker (has veclike span but needs fields).
    Marker(DumpMarker),
    /// Free slot.
    Free,
}

// ---------------------------------------------------------------------------
// Build (dump path)
// ---------------------------------------------------------------------------

/// Build the ObjectExtra section bytes from dump heap objects.
pub(crate) fn build_object_extra(objects: &[DumpHeapObject]) -> Result<Vec<u8>, DumpError> {
    let mut bytes = vec![0u8; HEADER_SIZE];
    for (index, obj) in objects.iter().enumerate() {
        if !object_needs_extra(obj) {
            continue;
        }
        write_dump_usize(&mut bytes, index, "object-extra object index")?;
        write_object_extra(&mut bytes, obj)?;
    }
    let payload_len = bytes.len() - HEADER_SIZE;
    let header = ObjectExtraHeader {
        magic: OBJECT_EXTRA_MAGIC,
        version: OBJECT_EXTRA_FORMAT_VERSION,
        header_size: HEADER_SIZE as u32,
        object_count: objects.len() as u64,
        payload_offset: HEADER_SIZE as u64,
        payload_len: payload_len as u64,
    };
    bytes[..HEADER_SIZE].copy_from_slice(bytemuck::bytes_of(&header));
    Ok(bytes)
}

fn write_object_extra(out: &mut Vec<u8>, obj: &DumpHeapObject) -> Result<(), DumpError> {
    match obj {
        DumpHeapObject::Cons { .. }
        | DumpHeapObject::Float(_)
        | DumpHeapObject::Vector(_)
        | DumpHeapObject::Lambda(_)
        | DumpHeapObject::Macro(_)
        | DumpHeapObject::Record(_) => {}
        // Category B: partial extra data.
        DumpHeapObject::Str {
            data,
            size,
            size_byte,
            text_props,
        } => {
            object_value_codec::write_u8(out, EXTRA_STRING);
            write_dump_usize(out, *size, "string size")?;
            write_dump_i32(out, *size_byte, "string size_byte")?;
            // Write byte data (Owned or Mapped)
            match data {
                DumpByteData::Owned(bytes) => {
                    object_value_codec::write_u8(out, 0);
                    write_dump_usize(out, bytes.len(), "string owned byte length")?;
                    out.extend_from_slice(bytes);
                }
                DumpByteData::Mapped(span) => {
                    object_value_codec::write_u8(out, 1);
                    write_dump_u64(out, span.offset, "string mapped offset")?;
                    write_dump_u64(out, span.len, "string mapped length")?;
                }
                DumpByteData::StaticRoData { key, len } => {
                    object_value_codec::write_u8(out, 2);
                    object_value_codec::write_u64(out, *key);
                    object_value_codec::write_u64(out, *len);
                }
            }
            write_text_property_runs(out, text_props)?;
        }
        DumpHeapObject::Overlay(overlay) => {
            object_value_codec::write_u8(out, EXTRA_OVERLAY);
            object_value_codec::write_heap_object(out, &DumpHeapObject::Overlay(overlay.clone()))?;
        }
        DumpHeapObject::Marker(marker) => {
            object_value_codec::write_u8(out, EXTRA_MARKER);
            object_value_codec::write_heap_object(out, &DumpHeapObject::Marker(marker.clone()))?;
        }
        // Category C: full descriptor (no HeapImage bytes).
        DumpHeapObject::HashTable(table) => {
            object_value_codec::write_u8(out, EXTRA_HASH_TABLE);
            object_value_codec::write_heap_object(out, &DumpHeapObject::HashTable(table.clone()))?;
        }
        DumpHeapObject::ByteCode(function) => {
            object_value_codec::write_u8(out, EXTRA_BYTE_CODE);
            object_value_codec::write_heap_object(
                out,
                &DumpHeapObject::ByteCode(function.clone()),
            )?;
        }
        DumpHeapObject::Subr {
            name,
            min_args,
            max_args,
        } => {
            object_value_codec::write_u8(out, EXTRA_SUBR);
            object_value_codec::write_u32(out, name.0);
            object_value_codec::write_u16(out, *min_args);
            write_opt_u16(out, *max_args);
        }
        DumpHeapObject::Buffer(id) => {
            object_value_codec::write_u8(out, EXTRA_BUFFER);
            object_value_codec::write_u64(out, id.0);
        }
        DumpHeapObject::Window(id) => {
            object_value_codec::write_u8(out, EXTRA_WINDOW);
            object_value_codec::write_u64(out, *id);
        }
        DumpHeapObject::Frame(id) => {
            object_value_codec::write_u8(out, EXTRA_FRAME);
            object_value_codec::write_u64(out, *id);
        }
        DumpHeapObject::Timer(id) => {
            object_value_codec::write_u8(out, EXTRA_TIMER);
            object_value_codec::write_u64(out, *id);
        }
        DumpHeapObject::Free => {
            object_value_codec::write_u8(out, EXTRA_FREE);
        }
    }
    Ok(())
}

fn object_needs_extra(obj: &DumpHeapObject) -> bool {
    !matches!(
        obj,
        DumpHeapObject::Cons { .. }
            | DumpHeapObject::Float(_)
            | DumpHeapObject::Vector(_)
            | DumpHeapObject::Lambda(_)
            | DumpHeapObject::Macro(_)
            | DumpHeapObject::Record(_)
    )
}

// ---------------------------------------------------------------------------
// Load (load path)
// ---------------------------------------------------------------------------

/// Load the sparse ObjectExtra section into the present extra records.
pub(crate) fn load_object_extra(section: &[u8]) -> Result<Vec<ObjectExtra>, DumpError> {
    let (_count, payload) = object_extra_payload(section)?;
    let mut cursor = object_value_codec::Cursor::new_at(payload, 0);
    let mut extras = Vec::new();
    while !cursor.is_empty() {
        let _index = read_dump_usize(&mut cursor, "object-extra object index")?;
        extras.push(read_object_extra(&mut cursor)?);
    }
    Ok(extras)
}

/// Load ObjectExtra for the file pdump path without expanding mapped
/// vectorlike objects into large nil-filled placeholder slot vectors.
///
/// GNU's pdumper does not serialize semantic descriptors for objects already in
/// the mapped image. Neomacs still needs a per-object descriptor vector while
/// the loader is transitional, but Category A descriptors are synthesized from
/// ObjectStarts and mapped heap headers.
pub(crate) fn load_compact_heap_objects_from_object_extra(
    section: &[u8],
    spans: &LoadedSpans<'_>,
    mapped_heap: Option<MappedHeapView>,
) -> Result<Vec<DumpHeapObject>, DumpError> {
    let (count, payload) = object_extra_payload(section)?;
    if spans.len() != count {
        return Err(DumpError::ImageFormatError(format!(
            "object-extra count {count} does not match object-starts count {}",
            spans.len()
        )));
    }
    let mut objects = Vec::with_capacity(count);
    for index in 0..count {
        objects.push(mapped_object_from_span(spans.get(index), mapped_heap)?);
    }

    let mut cursor = object_value_codec::Cursor::new_at(payload, 0);
    while !cursor.is_empty() {
        let index = read_dump_usize(&mut cursor, "object-extra object index")?;
        if index >= count {
            return Err(DumpError::ImageFormatError(format!(
                "object-extra index {index} is outside object count {count}"
            )));
        }
        if objects[index].is_some() {
            return Err(DumpError::ImageFormatError(format!(
                "object-extra has duplicate or unnecessary record for mapped object {index}"
            )));
        }
        let extra = read_object_extra(&mut cursor)?;
        objects[index] = Some(object_extra_into_heap_object(extra));
    }

    objects
        .into_iter()
        .enumerate()
        .map(|(index, object)| {
            object.ok_or_else(|| {
                DumpError::ImageFormatError(format!(
                    "object-extra has no descriptor for object {index}"
                ))
            })
        })
        .collect()
}

fn object_extra_payload(section: &[u8]) -> Result<(usize, &[u8]), DumpError> {
    if section.len() < HEADER_SIZE {
        return Err(DumpError::ImageFormatError(
            "object-extra section too small for header".into(),
        ));
    }
    let header = *bytemuck::from_bytes::<ObjectExtraHeader>(&section[..HEADER_SIZE]);
    if header.magic != OBJECT_EXTRA_MAGIC {
        return Err(DumpError::ImageFormatError(
            "object-extra magic mismatch".into(),
        ));
    }
    if header.version != OBJECT_EXTRA_FORMAT_VERSION {
        return Err(DumpError::ImageFormatError(format!(
            "object-extra version mismatch: expected {}, got {}",
            OBJECT_EXTRA_FORMAT_VERSION, header.version,
        )));
    }
    let count = header.object_count as usize;
    let payload_start = header.payload_offset as usize;
    let payload_end = payload_start + header.payload_len as usize;
    if payload_end > section.len() {
        return Err(DumpError::ImageFormatError(
            "object-extra payload extends past section".into(),
        ));
    }

    Ok((count, &section[payload_start..payload_end]))
}

fn object_extra_into_heap_object(extra: ObjectExtra) -> DumpHeapObject {
    match extra {
        ObjectExtra::String {
            size,
            size_byte,
            byte_data,
            text_props,
        } => DumpHeapObject::Str {
            data: byte_data,
            size,
            size_byte,
            text_props,
        },
        ObjectExtra::HashTable(table) => DumpHeapObject::HashTable(table),
        ObjectExtra::ByteCode(function) => DumpHeapObject::ByteCode(function),
        ObjectExtra::Subr {
            name,
            min_args,
            max_args,
        } => DumpHeapObject::Subr {
            name,
            min_args,
            max_args,
        },
        ObjectExtra::Buffer(id) => DumpHeapObject::Buffer(id),
        ObjectExtra::Window(id) => DumpHeapObject::Window(id),
        ObjectExtra::Frame(id) => DumpHeapObject::Frame(id),
        ObjectExtra::Timer(id) => DumpHeapObject::Timer(id),
        ObjectExtra::Overlay(overlay) => DumpHeapObject::Overlay(overlay),
        ObjectExtra::Marker(marker) => DumpHeapObject::Marker(marker),
        ObjectExtra::Free => DumpHeapObject::Free,
    }
}

fn mapped_object_from_span(
    span: LoadedObjectSpan,
    mapped_heap: Option<MappedHeapView>,
) -> Result<Option<DumpHeapObject>, DumpError> {
    match span {
        LoadedObjectSpan::Cons(_) => Ok(Some(DumpHeapObject::Cons {
            car: DumpValue::Nil,
            cdr: DumpValue::Nil,
        })),
        LoadedObjectSpan::Float(_) => Ok(Some(DumpHeapObject::Float(0.0))),
        LoadedObjectSpan::Vectorlike { object, .. } => {
            let mapped_heap = mapped_heap.ok_or_else(|| {
                DumpError::ImageFormatError(
                    "mapped vectorlike span requires a heap image section".into(),
                )
            })?;
            match mapped_heap.veclike_type(object)? {
                VecLikeType::Vector => Ok(Some(DumpHeapObject::Vector(Vec::new()))),
                VecLikeType::Lambda => Ok(Some(DumpHeapObject::Lambda(Vec::new()))),
                VecLikeType::Macro => Ok(Some(DumpHeapObject::Macro(Vec::new()))),
                VecLikeType::Record => Ok(Some(DumpHeapObject::Record(Vec::new()))),
                VecLikeType::Marker | VecLikeType::Overlay => Ok(None),
                other => Err(DumpError::ImageFormatError(format!(
                    "unexpected mapped vectorlike type {other:?} in object-starts"
                ))),
            }
        }
        LoadedObjectSpan::None | LoadedObjectSpan::String(_) | LoadedObjectSpan::Unmapped => {
            Ok(None)
        }
    }
}

fn read_object_extra(cursor: &mut object_value_codec::Cursor) -> Result<ObjectExtra, DumpError> {
    let tag = cursor.read_u8("object extra tag")?;
    match tag {
        EXTRA_STRING => {
            let size = read_dump_usize(cursor, "string size")?;
            let size_byte = read_dump_i32(cursor, "string size_byte")?;
            let byte_data_tag = cursor.read_u8("string byte data tag")?;
            let byte_data = match byte_data_tag {
                0 => {
                    let len = read_dump_usize(cursor, "string owned len")?;
                    let bytes = cursor.read_bytes_fixed(len)?;
                    DumpByteData::owned(bytes)
                }
                1 => {
                    let offset = read_dump_u64(cursor, "string mapped offset")?;
                    let len = read_dump_u64(cursor, "string mapped len")?;
                    DumpByteData::mapped(offset, len)
                }
                2 => {
                    let key = cursor.read_u64("string static rodata key")?;
                    let len = cursor.read_u64("string static rodata len")?;
                    DumpByteData::static_rodata(key, len)
                }
                other => {
                    return Err(DumpError::ImageFormatError(format!(
                        "unknown string byte data tag {other}"
                    )));
                }
            };
            let text_props = read_text_property_runs(cursor)?;
            Ok(ObjectExtra::String {
                size,
                size_byte,
                byte_data,
                text_props,
            })
        }
        EXTRA_HASH_TABLE => {
            // Skip the HEAP_HASH_TABLE tag written by write_heap_object
            let obj = cursor.read_heap_object()?;
            match obj {
                DumpHeapObject::HashTable(table) => Ok(ObjectExtra::HashTable(table)),
                other => Err(DumpError::ImageFormatError(format!(
                    "expected HashTable in ObjectExtra, got {:?}",
                    other.variant_name()
                ))),
            }
        }
        EXTRA_BYTE_CODE => {
            let obj = cursor.read_heap_object()?;
            match obj {
                DumpHeapObject::ByteCode(function) => Ok(ObjectExtra::ByteCode(function)),
                other => Err(DumpError::ImageFormatError(format!(
                    "expected ByteCode in ObjectExtra, got {:?}",
                    other.variant_name()
                ))),
            }
        }
        EXTRA_SUBR => {
            let name = DumpNameId(cursor.read_u32("subr name id")?);
            let min_args = cursor.read_u16("subr min args")?;
            let max_args = cursor.read_opt_u16()?;
            Ok(ObjectExtra::Subr {
                name,
                min_args,
                max_args,
            })
        }
        EXTRA_BUFFER => {
            let id = DumpBufferId(cursor.read_u64("buffer id")?);
            Ok(ObjectExtra::Buffer(id))
        }
        EXTRA_WINDOW => {
            let id = cursor.read_u64("window id")?;
            Ok(ObjectExtra::Window(id))
        }
        EXTRA_FRAME => {
            let id = cursor.read_u64("frame id")?;
            Ok(ObjectExtra::Frame(id))
        }
        EXTRA_TIMER => {
            let id = cursor.read_u64("timer id")?;
            Ok(ObjectExtra::Timer(id))
        }
        EXTRA_OVERLAY => {
            let obj = cursor.read_heap_object()?;
            match obj {
                DumpHeapObject::Overlay(overlay) => Ok(ObjectExtra::Overlay(overlay)),
                other => Err(DumpError::ImageFormatError(format!(
                    "expected Overlay in ObjectExtra, got {:?}",
                    other.variant_name()
                ))),
            }
        }
        EXTRA_MARKER => {
            let obj = cursor.read_heap_object()?;
            match obj {
                DumpHeapObject::Marker(marker) => Ok(ObjectExtra::Marker(marker)),
                other => Err(DumpError::ImageFormatError(format!(
                    "expected Marker in ObjectExtra, got {:?}",
                    other.variant_name()
                ))),
            }
        }
        EXTRA_FREE => Ok(ObjectExtra::Free),
        other => Err(DumpError::ImageFormatError(format!(
            "unknown object-extra tag {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Write helpers
// ---------------------------------------------------------------------------

fn write_opt_u16(out: &mut Vec<u8>, value: Option<u16>) {
    match value {
        Some(v) => {
            object_value_codec::write_u8(out, 1);
            object_value_codec::write_u16(out, v);
        }
        None => object_value_codec::write_u8(out, 0),
    }
}

fn write_text_property_runs(
    out: &mut Vec<u8>,
    runs: &[DumpStringTextPropertyRun],
) -> Result<(), DumpError> {
    write_dump_usize(out, runs.len(), "string text property run count")?;
    for run in runs {
        write_dump_usize(out, run.start, "string text property start")?;
        write_dump_usize(out, run.end, "string text property end")?;
        object_value_codec::write_value(out, &run.plist)?;
    }
    Ok(())
}

fn read_text_property_runs(
    cursor: &mut object_value_codec::Cursor,
) -> Result<Vec<DumpStringTextPropertyRun>, DumpError> {
    let len = read_dump_usize(cursor, "string text property run count")?;
    let mut runs = Vec::with_capacity(len);
    for _ in 0..len {
        runs.push(DumpStringTextPropertyRun {
            start: read_dump_usize(cursor, "string text property start")?,
            end: read_dump_usize(cursor, "string text property end")?,
            plist: cursor.read_value()?,
        });
    }
    Ok(runs)
}

fn write_dump_usize(out: &mut Vec<u8>, value: usize, what: &str) -> Result<(), DumpError> {
    let value = u32::try_from(value)
        .map_err(|_| DumpError::SerializationError(format!("{what} overflows dump_off")))?;
    object_value_codec::write_u32(out, value);
    Ok(())
}

fn write_dump_u64(out: &mut Vec<u8>, value: u64, what: &str) -> Result<(), DumpError> {
    let value = u32::try_from(value)
        .map_err(|_| DumpError::SerializationError(format!("{what} overflows dump_off")))?;
    object_value_codec::write_u32(out, value);
    Ok(())
}

fn write_dump_i32(out: &mut Vec<u8>, value: i64, what: &str) -> Result<(), DumpError> {
    let value = i32::try_from(value)
        .map_err(|_| DumpError::SerializationError(format!("{what} overflows dump_off")))?;
    out.extend_from_slice(&value.to_ne_bytes());
    Ok(())
}

fn read_dump_usize(
    cursor: &mut object_value_codec::Cursor,
    what: &str,
) -> Result<usize, DumpError> {
    Ok(cursor.read_u32(what)? as usize)
}

fn read_dump_u64(cursor: &mut object_value_codec::Cursor, what: &str) -> Result<u64, DumpError> {
    Ok(u64::from(cursor.read_u32(what)?))
}

fn read_dump_i32(cursor: &mut object_value_codec::Cursor, what: &str) -> Result<i64, DumpError> {
    let raw = cursor.read_u32(what)?;
    Ok(i64::from(i32::from_ne_bytes(raw.to_ne_bytes())))
}

// ---------------------------------------------------------------------------
// DumpHeapObject helper
// ---------------------------------------------------------------------------

impl DumpHeapObject {
    fn variant_name(&self) -> &'static str {
        match self {
            DumpHeapObject::Cons { .. } => "Cons",
            DumpHeapObject::Vector(_) => "Vector",
            DumpHeapObject::HashTable(_) => "HashTable",
            DumpHeapObject::Str { .. } => "Str",
            DumpHeapObject::Float(_) => "Float",
            DumpHeapObject::Lambda(_) => "Lambda",
            DumpHeapObject::Macro(_) => "Macro",
            DumpHeapObject::ByteCode(_) => "ByteCode",
            DumpHeapObject::Record(_) => "Record",
            DumpHeapObject::Marker(_) => "Marker",
            DumpHeapObject::Overlay(_) => "Overlay",
            DumpHeapObject::Buffer(_) => "Buffer",
            DumpHeapObject::Window(_) => "Window",
            DumpHeapObject::Frame(_) => "Frame",
            DumpHeapObject::Timer(_) => "Timer",
            DumpHeapObject::Subr { .. } => "Subr",
            DumpHeapObject::Free => "Free",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::header::{GcHeader, LambdaObj, MacroObj, RecordObj, VectorObj};

    #[test]
    fn object_extra_is_sparse_for_category_a_descriptors() {
        let bytes = build_object_extra(&[
            DumpHeapObject::Cons {
                car: DumpValue::Nil,
                cdr: DumpValue::True,
            },
            DumpHeapObject::Vector(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Free,
        ])
        .expect("build object extra");

        let extras = load_object_extra(&bytes).expect("load object extra");
        assert_eq!(extras.len(), 1);
        assert!(matches!(extras[0], ObjectExtra::Free));
    }

    #[test]
    fn object_extra_loads_sparse_heap_objects_from_spans() {
        let objects = vec![
            DumpHeapObject::Cons {
                car: DumpValue::True,
                cdr: DumpValue::Nil,
            },
            DumpHeapObject::Free,
        ];
        let bytes = build_object_extra(&objects).expect("build object extra");
        let heap = DumpTaggedHeap {
            objects,
            mapped_cons: vec![Some(DumpConsSpan { offset: 0 }), None],
            mapped_floats: vec![None, None],
            mapped_strings: vec![None, None],
            mapped_veclikes: vec![None, None],
            mapped_slots: vec![None, None],
        };
        let spans = LoadedSpans::from_heap(&heap);

        let objects = load_compact_heap_objects_from_object_extra(&bytes, &spans, None)
            .expect("load heap objects from sparse extra");

        assert!(matches!(objects[0], DumpHeapObject::Cons { .. }));
        assert!(matches!(objects[1], DumpHeapObject::Free));
    }

    #[test]
    fn object_extra_round_trips_static_rodata_string_descriptor() {
        let objects = vec![DumpHeapObject::Str {
            data: DumpByteData::static_rodata(0x1234_5678, 7),
            size: 7,
            size_byte: -2,
            text_props: Vec::new(),
        }];
        let bytes = build_object_extra(&objects).expect("build object extra");
        let extras = load_object_extra(&bytes).expect("load object extra");

        assert!(matches!(
            &extras[0],
            ObjectExtra::String {
                size: 7,
                size_byte: -2,
                byte_data: DumpByteData::StaticRoData { key: 0x1234_5678, len: 7 },
                text_props,
            } if text_props.is_empty()
        ));
    }

    #[test]
    fn compact_object_extra_infers_mapped_vectorlike_descriptors_from_headers() {
        let objects = vec![
            DumpHeapObject::Vector(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Lambda(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Macro(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Record(vec![DumpValue::Nil, DumpValue::True]),
        ];
        let bytes = build_object_extra(&objects).expect("build object extra");
        assert_eq!(bytes.len(), HEADER_SIZE);

        let mut offset = 0u64;
        let vector_span = reserve_test_object::<VectorObj>(&mut offset);
        let lambda_span = reserve_test_object::<LambdaObj>(&mut offset);
        let macro_span = reserve_test_object::<MacroObj>(&mut offset);
        let record_span = reserve_test_object::<RecordObj>(&mut offset);
        let mut heap_bytes = vec![0u8; offset as usize];
        write_test_veclike_type(&mut heap_bytes, vector_span, VecLikeType::Vector);
        write_test_veclike_type(&mut heap_bytes, lambda_span, VecLikeType::Lambda);
        write_test_veclike_type(&mut heap_bytes, macro_span, VecLikeType::Macro);
        write_test_veclike_type(&mut heap_bytes, record_span, VecLikeType::Record);

        let heap = DumpTaggedHeap {
            objects,
            mapped_cons: vec![None; 4],
            mapped_floats: vec![None; 4],
            mapped_strings: vec![None; 4],
            mapped_veclikes: vec![
                Some(vector_span),
                Some(lambda_span),
                Some(macro_span),
                Some(record_span),
            ],
            mapped_slots: vec![None; 4],
        };
        let spans = LoadedSpans::from_heap(&heap);
        let mapped_heap = MappedHeapView::from_mut_slice(&mut heap_bytes);

        let objects =
            load_compact_heap_objects_from_object_extra(&bytes, &spans, Some(mapped_heap))
                .expect("load compact heap objects from extra");

        assert!(matches!(objects[0], DumpHeapObject::Vector(ref slots) if slots.is_empty()));
        assert!(matches!(objects[1], DumpHeapObject::Lambda(ref slots) if slots.is_empty()));
        assert!(matches!(objects[2], DumpHeapObject::Macro(ref slots) if slots.is_empty()));
        assert!(matches!(objects[3], DumpHeapObject::Record(ref slots) if slots.is_empty()));
    }

    #[test]
    fn object_extra_rejects_removed_none_tag() {
        let mut bytes = build_object_extra(&[DumpHeapObject::Free]).expect("build object extra");
        bytes[HEADER_SIZE + 4] = 100;

        let err = load_object_extra(&bytes).expect_err("removed NONE tag should be rejected");
        assert!(matches!(err, DumpError::ImageFormatError(_)));
    }

    fn reserve_test_object<T>(offset: &mut u64) -> DumpVecLikeSpan {
        let span = DumpVecLikeSpan {
            offset: *offset,
            len: std::mem::size_of::<T>() as u64,
        };
        *offset += span.len;
        span
    }

    fn write_test_veclike_type(bytes: &mut [u8], span: DumpVecLikeSpan, type_tag: VecLikeType) {
        bytes[span.offset as usize + std::mem::size_of::<GcHeader>()] = type_tag as u8;
    }
}
