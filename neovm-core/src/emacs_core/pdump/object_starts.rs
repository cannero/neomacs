//! ObjectStarts section: maps object index → HeapImage offset + span metadata.
//!
//! During dump, the span tables (mapped_cons, mapped_floats, mapped_strings,
//! mapped_veclikes, mapped_slots) are computed and stored directly in this
//! section. During load, they are read back directly, eliminating the need
//! to re-run the layout algorithm via `rebuild_heap_metadata`.

use bytemuck::{Pod, Zeroable};

use super::{DumpError, types::*};

const OBJECT_STARTS_MAGIC: [u8; 16] = *b"NEOOBJSTARTS\0\0\0\0";
const OBJECT_STARTS_FORMAT_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ObjectStartsHeader {
    magic: [u8; 16],
    version: u32,
    header_size: u32,
    object_count: u64,
}

const HEADER_SIZE: usize = std::mem::size_of::<ObjectStartsHeader>();

/// Build the ObjectStarts section bytes from the dump tagged heap.
///
/// Encodes the span tables compactly: for each object, a u8 type tag
/// followed by type-specific span data. Objects with no span store
/// just a tag byte (type = 0).
pub(crate) fn build_object_starts(heap: &DumpTaggedHeap) -> Result<Vec<u8>, DumpError> {
    let count = heap.objects.len();
    let mut bytes = vec![0u8; HEADER_SIZE];

    for (i, obj) in heap.objects.iter().enumerate() {
        write_object_span(&mut bytes, obj, heap, i);
    }

    let header = ObjectStartsHeader {
        magic: OBJECT_STARTS_MAGIC,
        version: OBJECT_STARTS_FORMAT_VERSION,
        header_size: HEADER_SIZE as u32,
        object_count: count as u64,
    };
    bytes[..HEADER_SIZE].copy_from_slice(bytemuck::bytes_of(&header));
    Ok(bytes)
}

// Type tags for span records.
const SPAN_NONE: u8 = 0;
const SPAN_CONS: u8 = 1;
const SPAN_FLOAT: u8 = 2;
const SPAN_STRING: u8 = 3;
const SPAN_VECTORLIKE: u8 = 4;
// Category C objects (no span).
const SPAN_UNMAPPED: u8 = 5;

fn write_object_span(out: &mut Vec<u8>, obj: &DumpHeapObject, heap: &DumpTaggedHeap, index: usize) {
    match obj {
        DumpHeapObject::Cons { .. } => {
            if let Some(span) = heap.mapped_cons.get(index).and_then(|s| *s) {
                out.push(SPAN_CONS);
                write_u64(out, span.offset);
            } else {
                out.push(SPAN_NONE);
            }
        }
        DumpHeapObject::Float(_) => {
            if let Some(span) = heap.mapped_floats.get(index).and_then(|s| *s) {
                out.push(SPAN_FLOAT);
                write_u64(out, span.offset);
            } else {
                out.push(SPAN_NONE);
            }
        }
        DumpHeapObject::Str { .. } => {
            if let Some(span) = heap.mapped_strings.get(index).and_then(|s| *s) {
                out.push(SPAN_STRING);
                write_u64(out, span.offset);
                write_u64(out, span.len);
            } else {
                out.push(SPAN_NONE);
            }
        }
        DumpHeapObject::Vector(_)
        | DumpHeapObject::Lambda(_)
        | DumpHeapObject::Macro(_)
        | DumpHeapObject::Record(_)
        | DumpHeapObject::Marker(_)
        | DumpHeapObject::Overlay(_) => {
            let vl = heap.mapped_veclikes.get(index).and_then(|s| *s);
            let sl = heap.mapped_slots.get(index).and_then(|s| *s);
            if let Some(vl) = vl {
                out.push(SPAN_VECTORLIKE);
                write_u64(out, vl.offset);
                write_u64(out, vl.len);
                if let Some(sl) = sl {
                    out.push(1); // has slots
                    write_u64(out, sl.offset);
                    write_u64(out, sl.len);
                } else {
                    out.push(0); // no slots
                }
            } else {
                out.push(SPAN_NONE);
            }
        }
        // Category C: no HeapImage representation.
        DumpHeapObject::HashTable(_)
        | DumpHeapObject::ByteCode(_)
        | DumpHeapObject::Subr { .. }
        | DumpHeapObject::Buffer(_)
        | DumpHeapObject::Window(_)
        | DumpHeapObject::Frame(_)
        | DumpHeapObject::Timer(_)
        | DumpHeapObject::Free => {
            out.push(SPAN_UNMAPPED);
        }
    }
}

/// Load the ObjectStarts section and reconstruct span tables.
///
/// Returns the span tables needed by the load path, avoiding the need
/// for `rebuild_heap_metadata`.
pub(crate) struct LoadedSpans {
    pub mapped_cons: Vec<Option<DumpConsSpan>>,
    pub mapped_floats: Vec<Option<DumpFloatSpan>>,
    pub mapped_strings: Vec<Option<DumpStringSpan>>,
    pub mapped_veclikes: Vec<Option<DumpVecLikeSpan>>,
    pub mapped_slots: Vec<Option<DumpSlotSpan>>,
}

pub(crate) fn load_object_starts(section: &[u8]) -> Result<LoadedSpans, DumpError> {
    if section.len() < HEADER_SIZE {
        return Err(DumpError::ImageFormatError(
            "object-starts section too small for header".into(),
        ));
    }
    let header = *bytemuck::from_bytes::<ObjectStartsHeader>(&section[..HEADER_SIZE]);
    if header.magic != OBJECT_STARTS_MAGIC {
        return Err(DumpError::ImageFormatError(
            "object-starts magic mismatch".into(),
        ));
    }
    if header.version != OBJECT_STARTS_FORMAT_VERSION {
        return Err(DumpError::ImageFormatError(format!(
            "object-starts version mismatch: expected {}, got {}",
            OBJECT_STARTS_FORMAT_VERSION, header.version,
        )));
    }
    let count = header.object_count as usize;
    let mut cursor = HEADER_SIZE;
    let mut mapped_cons = vec![None; count];
    let mut mapped_floats = vec![None; count];
    let mut mapped_strings = vec![None; count];
    let mut mapped_veclikes = vec![None; count];
    let mut mapped_slots = vec![None; count];

    for i in 0..count {
        if cursor >= section.len() {
            return Err(DumpError::ImageFormatError(
                "object-starts section truncated".into(),
            ));
        }
        let tag = section[cursor];
        cursor += 1;
        match tag {
            SPAN_NONE | SPAN_UNMAPPED => {}
            SPAN_CONS => {
                let offset = read_u64(section, &mut cursor)?;
                mapped_cons[i] = Some(DumpConsSpan { offset });
            }
            SPAN_FLOAT => {
                let offset = read_u64(section, &mut cursor)?;
                mapped_floats[i] = Some(DumpFloatSpan { offset });
            }
            SPAN_STRING => {
                let offset = read_u64(section, &mut cursor)?;
                let len = read_u64(section, &mut cursor)?;
                mapped_strings[i] = Some(DumpStringSpan { offset, len });
            }
            SPAN_VECTORLIKE => {
                let vl_offset = read_u64(section, &mut cursor)?;
                let vl_len = read_u64(section, &mut cursor)?;
                mapped_veclikes[i] = Some(DumpVecLikeSpan {
                    offset: vl_offset,
                    len: vl_len,
                });
                if cursor >= section.len() {
                    return Err(DumpError::ImageFormatError(
                        "object-starts vectorlike slot flag truncated".into(),
                    ));
                }
                let has_slots = section[cursor];
                cursor += 1;
                if has_slots != 0 {
                    let sl_offset = read_u64(section, &mut cursor)?;
                    let sl_len = read_u64(section, &mut cursor)?;
                    mapped_slots[i] = Some(DumpSlotSpan {
                        offset: sl_offset,
                        len: sl_len,
                    });
                }
            }
            other => {
                return Err(DumpError::ImageFormatError(format!(
                    "unknown object-starts span tag {other}"
                )));
            }
        }
    }

    Ok(LoadedSpans {
        mapped_cons,
        mapped_floats,
        mapped_strings,
        mapped_veclikes,
        mapped_slots,
    })
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u64(data: &[u8], cursor: &mut usize) -> Result<u64, DumpError> {
    if *cursor + 8 > data.len() {
        return Err(DumpError::ImageFormatError(
            "object-starts section truncated at u64".into(),
        ));
    }
    let val = u64::from_le_bytes(data[*cursor..*cursor + 8].try_into().unwrap());
    *cursor += 8;
    Ok(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_starts_round_trips() {
        let heap = DumpTaggedHeap {
            objects: vec![
                DumpHeapObject::Cons {
                    car: DumpValue::Int(1),
                    cdr: DumpValue::Nil,
                },
                DumpHeapObject::Float(3.14),
                DumpHeapObject::Free,
                DumpHeapObject::Vector(vec![DumpValue::Nil, DumpValue::True]),
                DumpHeapObject::Str {
                    data: DumpByteData::owned(b"hello".to_vec()),
                    size: 5,
                    size_byte: 5,
                    text_props: vec![],
                },
            ],
            mapped_cons: vec![Some(DumpConsSpan { offset: 0 }), None, None, None, None],
            mapped_floats: vec![None, Some(DumpFloatSpan { offset: 32 }), None, None, None],
            mapped_strings: vec![
                None,
                None,
                None,
                None,
                Some(DumpStringSpan {
                    offset: 48,
                    len: 16,
                }),
            ],
            mapped_veclikes: vec![
                None,
                None,
                None,
                Some(DumpVecLikeSpan {
                    offset: 64,
                    len: 24,
                }),
                None,
            ],
            mapped_slots: vec![
                None,
                None,
                None,
                Some(DumpSlotSpan {
                    offset: 88,
                    len: 16,
                }),
                None,
            ],
        };
        let bytes = build_object_starts(&heap).unwrap();
        let spans = load_object_starts(&bytes).unwrap();
        assert_eq!(spans.mapped_cons.len(), 5);
        assert_eq!(spans.mapped_cons[0], Some(DumpConsSpan { offset: 0 }));
        assert!(spans.mapped_cons[1].is_none());
        assert_eq!(spans.mapped_floats[1], Some(DumpFloatSpan { offset: 32 }));
        assert_eq!(
            spans.mapped_strings[4],
            Some(DumpStringSpan {
                offset: 48,
                len: 16
            })
        );
        assert_eq!(
            spans.mapped_veclikes[3],
            Some(DumpVecLikeSpan {
                offset: 64,
                len: 24
            })
        );
        assert_eq!(
            spans.mapped_slots[3],
            Some(DumpSlotSpan {
                offset: 88,
                len: 16
            })
        );
        assert!(spans.mapped_cons[2].is_none()); // Free
    }
}
