//! Compact ObjectExtra section: per-object extra data for Category B/C objects.
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
//! Serialization strategy: the extra tag byte identifies the variant, then
//! the payload uses the same encoding as `object_value_codec::write_heap_object`
//! for complex types. On read, we delegate to `Cursor::read_heap_object` and
//! extract the relevant fields from the returned `DumpHeapObject`.

use bytemuck::{Pod, Zeroable};

use super::object_value_codec;
use super::{DumpError, types::*};

const OBJECT_EXTRA_MAGIC: [u8; 16] = *b"NEOOBJEXTRA\0\0\0\0\0";
const OBJECT_EXTRA_FORMAT_VERSION: u32 = 3;

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
const EXTRA_CONS: u8 = 112;
const EXTRA_FLOAT: u8 = 113;
const EXTRA_VECTOR: u8 = 114;
const EXTRA_LAMBDA: u8 = 115;
const EXTRA_MACRO: u8 = 116;
const EXTRA_RECORD: u8 = 117;
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
    /// Category A: cons cell (data in HeapImage).
    Cons,
    /// Category A: float (data in HeapImage).
    Float,
    /// Category A: vector (object and slot count are in HeapImage/ObjectStarts).
    Vector,
    /// Category A: lambda (object and slot count are in HeapImage/ObjectStarts).
    Lambda,
    /// Category A: macro (object and slot count are in HeapImage/ObjectStarts).
    Macro,
    /// Category A: record (object and slot count are in HeapImage/ObjectStarts).
    Record,
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
    for obj in objects {
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
        // Category A: just the type tag. GNU pdumper keeps vector length in
        // the mapped object/slot layout, not in side metadata.
        DumpHeapObject::Cons { .. } => {
            object_value_codec::write_u8(out, EXTRA_CONS);
        }
        DumpHeapObject::Float(_) => {
            object_value_codec::write_u8(out, EXTRA_FLOAT);
        }
        DumpHeapObject::Vector(_) => {
            object_value_codec::write_u8(out, EXTRA_VECTOR);
        }
        DumpHeapObject::Lambda(_) => {
            object_value_codec::write_u8(out, EXTRA_LAMBDA);
        }
        DumpHeapObject::Macro(_) => {
            object_value_codec::write_u8(out, EXTRA_MACRO);
        }
        DumpHeapObject::Record(_) => {
            object_value_codec::write_u8(out, EXTRA_RECORD);
        }
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

// ---------------------------------------------------------------------------
// Load (load path)
// ---------------------------------------------------------------------------

/// Reconstruct a `Vec<DumpHeapObject>` from ObjectExtra + span tables.
///
/// Compatibility helper for tests and older callers. The main pdump load path
/// uses `load_heap_objects_from_object_extra` to avoid building this
/// intermediate vector.
///
/// Category A objects get placeholder data (the actual data comes from
/// HeapImage via relocations). Category B/C objects get their full
/// descriptor from ObjectExtra.
pub(crate) fn reconstruct_heap_objects(extras: &[ObjectExtra]) -> Vec<DumpHeapObject> {
    extras
        .iter()
        .cloned()
        .map(object_extra_into_heap_object)
        .collect()
}

/// Load the ObjectExtra section into per-object descriptors.
pub(crate) fn load_object_extra(section: &[u8]) -> Result<Vec<ObjectExtra>, DumpError> {
    let (count, payload) = object_extra_payload(section)?;
    let mut cursor = object_value_codec::Cursor::new_at(payload, 0);
    let mut extras = Vec::with_capacity(count);
    for _ in 0..count {
        let extra = read_object_extra(&mut cursor)?;
        extras.push(extra);
    }
    Ok(extras)
}

/// Load the ObjectExtra section directly into the object descriptors expected by
/// `LoadDecoder`.
///
/// This keeps the transitional descriptor vector that the current decoder still
/// requires, but skips the previous `Vec<ObjectExtra>` allocation and the clone
/// pass. GNU pdumper walks mapped metadata in place; this is a smaller step in
/// that direction while the decoder is still being retired.
pub(crate) fn load_heap_objects_from_object_extra(
    section: &[u8],
) -> Result<Vec<DumpHeapObject>, DumpError> {
    load_heap_objects_from_object_extra_with(section, object_extra_into_heap_object)
}

/// Load ObjectExtra for the file pdump path without expanding mapped
/// vectorlike objects into large nil-filled placeholder slot vectors.
///
/// GNU's pdumper does not rebuild vector slots from a semantic descriptor: the
/// slots already live in the mapped heap image. Neomacs still needs a
/// per-object descriptor vector while the loader is transitional, but for
/// Category A vectorlike objects that descriptor only needs the variant. The
/// authoritative slot count is read from ObjectStarts' mapped slot span during
/// load.
pub(crate) fn load_compact_heap_objects_from_object_extra(
    section: &[u8],
) -> Result<Vec<DumpHeapObject>, DumpError> {
    load_heap_objects_from_object_extra_with(section, object_extra_into_compact_heap_object)
}

fn load_heap_objects_from_object_extra_with(
    section: &[u8],
    mut convert: impl FnMut(ObjectExtra) -> DumpHeapObject,
) -> Result<Vec<DumpHeapObject>, DumpError> {
    let (count, payload) = object_extra_payload(section)?;
    let mut cursor = object_value_codec::Cursor::new_at(payload, 0);
    let mut objects = Vec::with_capacity(count);
    for _ in 0..count {
        let extra = read_object_extra(&mut cursor)?;
        objects.push(convert(extra));
    }
    Ok(objects)
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
        ObjectExtra::Cons => DumpHeapObject::Cons {
            car: DumpValue::Nil,
            cdr: DumpValue::Nil,
        },
        ObjectExtra::Float => DumpHeapObject::Float(0.0),
        ObjectExtra::Vector => DumpHeapObject::Vector(Vec::new()),
        ObjectExtra::Lambda => DumpHeapObject::Lambda(Vec::new()),
        ObjectExtra::Macro => DumpHeapObject::Macro(Vec::new()),
        ObjectExtra::Record => DumpHeapObject::Record(Vec::new()),
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

fn object_extra_into_compact_heap_object(extra: ObjectExtra) -> DumpHeapObject {
    match extra {
        ObjectExtra::Vector => DumpHeapObject::Vector(Vec::new()),
        ObjectExtra::Lambda => DumpHeapObject::Lambda(Vec::new()),
        ObjectExtra::Macro => DumpHeapObject::Macro(Vec::new()),
        ObjectExtra::Record => DumpHeapObject::Record(Vec::new()),
        other => object_extra_into_heap_object(other),
    }
}

fn read_object_extra(cursor: &mut object_value_codec::Cursor) -> Result<ObjectExtra, DumpError> {
    let tag = cursor.read_u8("object extra tag")?;
    match tag {
        EXTRA_CONS => Ok(ObjectExtra::Cons),
        EXTRA_FLOAT => Ok(ObjectExtra::Float),
        EXTRA_VECTOR => Ok(ObjectExtra::Vector),
        EXTRA_LAMBDA => Ok(ObjectExtra::Lambda),
        EXTRA_MACRO => Ok(ObjectExtra::Macro),
        EXTRA_RECORD => Ok(ObjectExtra::Record),
        EXTRA_STRING => {
            let size = read_dump_usize(cursor, "string size")?;
            let size_byte = read_dump_i32(cursor, "string size_byte")?;
            let byte_data_tag = cursor.read_u8("string byte data tag")?;
            let byte_data = if byte_data_tag == 0 {
                let len = read_dump_usize(cursor, "string owned len")?;
                let bytes = cursor.read_bytes_fixed(len)?;
                DumpByteData::owned(bytes)
            } else {
                let offset = read_dump_u64(cursor, "string mapped offset")?;
                let len = read_dump_u64(cursor, "string mapped len")?;
                DumpByteData::mapped(offset, len)
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

    #[test]
    fn object_extra_round_trips_category_a_and_free_descriptors() {
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
        assert!(matches!(extras[0], ObjectExtra::Cons));
        assert!(matches!(extras[1], ObjectExtra::Vector));
        assert!(matches!(extras[2], ObjectExtra::Free));
    }

    #[test]
    fn object_extra_loads_heap_objects_without_intermediate_extra_vector() {
        let bytes = build_object_extra(&[
            DumpHeapObject::Cons {
                car: DumpValue::True,
                cdr: DumpValue::Nil,
            },
            DumpHeapObject::Vector(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Free,
        ])
        .expect("build object extra");

        let objects =
            load_heap_objects_from_object_extra(&bytes).expect("load heap objects from extra");

        assert!(matches!(objects[0], DumpHeapObject::Cons { .. }));
        assert!(matches!(objects[1], DumpHeapObject::Vector(ref slots) if slots.is_empty()));
        assert!(matches!(objects[2], DumpHeapObject::Free));
    }

    #[test]
    fn compact_object_extra_keeps_mapped_vectorlike_descriptors_small() {
        let bytes = build_object_extra(&[
            DumpHeapObject::Vector(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Lambda(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Macro(vec![DumpValue::Nil, DumpValue::True]),
            DumpHeapObject::Record(vec![DumpValue::Nil, DumpValue::True]),
        ])
        .expect("build object extra");

        let objects = load_compact_heap_objects_from_object_extra(&bytes)
            .expect("load compact heap objects from extra");

        assert!(matches!(objects[0], DumpHeapObject::Vector(ref slots) if slots.is_empty()));
        assert!(matches!(objects[1], DumpHeapObject::Lambda(ref slots) if slots.is_empty()));
        assert!(matches!(objects[2], DumpHeapObject::Macro(ref slots) if slots.is_empty()));
        assert!(matches!(objects[3], DumpHeapObject::Record(ref slots) if slots.is_empty()));
    }

    #[test]
    fn object_extra_rejects_removed_none_tag() {
        let mut bytes = build_object_extra(&[DumpHeapObject::Free]).expect("build object extra");
        bytes[HEADER_SIZE] = 100;

        let err = load_object_extra(&bytes).expect_err("removed NONE tag should be rejected");
        assert!(matches!(err, DumpError::ImageFormatError(_)));
    }
}
