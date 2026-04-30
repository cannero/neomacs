//! ObjectStarts section: maps object index → HeapImage offset + span metadata.
//!
//! During dump, the span tables (mapped_cons, mapped_floats, mapped_strings,
//! mapped_veclikes, mapped_slots) are computed and stored directly in this
//! section. During load, they are read back directly, eliminating the need
//! to re-run the layout algorithm via `rebuild_heap_metadata`.

use bytemuck::{Pod, Zeroable};

use super::{DumpError, types::*};

const OBJECT_STARTS_MAGIC: [u8; 16] = *b"NEOOBJSTARTS\0\0\0\0";
const OBJECT_STARTS_FORMAT_VERSION: u32 = 4;

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
/// GNU pdumper keeps load metadata in the mapped image and walks it directly.
/// Keep this section compact, but make file pdump load borrow the mapped bytes
/// with a small object-index offset table instead of decoding every span into
/// Rust heap objects.
pub(crate) fn build_object_starts(heap: &DumpTaggedHeap) -> Result<Vec<u8>, DumpError> {
    let count = heap.objects.len();
    let mut bytes = vec![0u8; HEADER_SIZE];

    for (i, obj) in heap.objects.iter().enumerate() {
        write_object_span(&mut bytes, obj, heap, i)?;
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

fn write_object_span(
    out: &mut Vec<u8>,
    obj: &DumpHeapObject,
    heap: &DumpTaggedHeap,
    index: usize,
) -> Result<(), DumpError> {
    match obj {
        DumpHeapObject::Cons { .. } => {
            if let Some(span) = heap.mapped_cons.get(index).and_then(|s| *s) {
                out.push(SPAN_CONS);
                write_dump_off(out, span.offset)?;
            } else {
                out.push(SPAN_NONE);
            }
        }
        DumpHeapObject::Float(_) => {
            if let Some(span) = heap.mapped_floats.get(index).and_then(|s| *s) {
                out.push(SPAN_FLOAT);
                write_dump_off(out, span.offset)?;
            } else {
                out.push(SPAN_NONE);
            }
        }
        DumpHeapObject::Str { .. } => {
            if let Some(span) = heap.mapped_strings.get(index).and_then(|s| *s) {
                out.push(SPAN_STRING);
                write_dump_off(out, span.offset)?;
                write_dump_off(out, span.len)?;
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
                write_dump_off(out, vl.offset)?;
                write_dump_off(out, vl.len)?;
                if let Some(sl) = sl {
                    out.push(1); // has slots
                    write_dump_off(out, sl.offset)?;
                    write_dump_off(out, sl.len)?;
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
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadedObjectSpan {
    None,
    Unmapped,
    Cons(DumpConsSpan),
    Float(DumpFloatSpan),
    String(DumpStringSpan),
    Vectorlike {
        object: DumpVecLikeSpan,
        slots: Option<DumpSlotSpan>,
    },
}

impl Default for LoadedObjectSpan {
    fn default() -> Self {
        Self::None
    }
}

/// Load-side object span lookup.
///
/// GNU pdumper keeps the mapped dump as the primary object store and walks compact
/// relocation metadata at load time. Keep Neomacs' transitional span metadata in a
/// single object-indexed table instead of expanding it into five parallel
/// `Vec<Option<_>>` tables.
pub(crate) struct LoadedSpans<'a> {
    records: LoadedSpanRecords<'a>,
}

enum LoadedSpanRecords<'a> {
    Owned(Vec<LoadedObjectSpan>),
    Mapped { bytes: &'a [u8], offsets: Vec<u32> },
}

pub(crate) struct LoadedSpansIter<'spans, 'data> {
    spans: &'spans LoadedSpans<'data>,
    index: usize,
}

impl<'data> LoadedSpans<'data> {
    pub(crate) fn from_heap(heap: &DumpTaggedHeap) -> Self {
        let mut records = Vec::with_capacity(heap.objects.len());
        for index in 0..heap.objects.len() {
            records.push(span_record_from_heap(heap, index));
        }
        Self {
            records: LoadedSpanRecords::Owned(records),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match &self.records {
            LoadedSpanRecords::Owned(records) => records.len(),
            LoadedSpanRecords::Mapped { offsets, .. } => offsets.len(),
        }
    }

    pub(crate) fn get(&self, index: usize) -> LoadedObjectSpan {
        match &self.records {
            LoadedSpanRecords::Owned(records) => records.get(index).copied().unwrap_or_default(),
            LoadedSpanRecords::Mapped { bytes, offsets } => {
                let Some(offset) = offsets.get(index) else {
                    return LoadedObjectSpan::default();
                };
                loaded_span_at(bytes, *offset as usize).unwrap_or_default()
            }
        }
    }

    pub(crate) fn iter(&self) -> LoadedSpansIter<'_, 'data> {
        LoadedSpansIter {
            spans: self,
            index: 0,
        }
    }

    pub(crate) fn cons(&self, index: usize) -> Option<DumpConsSpan> {
        match self.get(index) {
            LoadedObjectSpan::Cons(span) => Some(span),
            _ => None,
        }
    }

    pub(crate) fn float(&self, index: usize) -> Option<DumpFloatSpan> {
        match self.get(index) {
            LoadedObjectSpan::Float(span) => Some(span),
            _ => None,
        }
    }

    pub(crate) fn string(&self, index: usize) -> Option<DumpStringSpan> {
        match self.get(index) {
            LoadedObjectSpan::String(span) => Some(span),
            _ => None,
        }
    }

    pub(crate) fn vectorlike(&self, index: usize) -> Option<DumpVecLikeSpan> {
        match self.get(index) {
            LoadedObjectSpan::Vectorlike { object, .. } => Some(object),
            _ => None,
        }
    }

    pub(crate) fn slots(&self, index: usize) -> Option<DumpSlotSpan> {
        match self.get(index) {
            LoadedObjectSpan::Vectorlike { slots, .. } => slots,
            _ => None,
        }
    }
}

impl Iterator for LoadedSpansIter<'_, '_> {
    type Item = (usize, LoadedObjectSpan);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.spans.len() {
            return None;
        }
        let index = self.index;
        self.index += 1;
        Some((index, self.spans.get(index)))
    }
}

fn span_record_from_heap(heap: &DumpTaggedHeap, index: usize) -> LoadedObjectSpan {
    if let Some(span) = heap.mapped_cons.get(index).copied().flatten() {
        return LoadedObjectSpan::Cons(span);
    }
    if let Some(span) = heap.mapped_floats.get(index).copied().flatten() {
        return LoadedObjectSpan::Float(span);
    }
    if let Some(span) = heap.mapped_strings.get(index).copied().flatten() {
        return LoadedObjectSpan::String(span);
    }
    if let Some(object) = heap.mapped_veclikes.get(index).copied().flatten() {
        return LoadedObjectSpan::Vectorlike {
            object,
            slots: heap.mapped_slots.get(index).copied().flatten(),
        };
    }
    match heap.objects.get(index) {
        Some(
            DumpHeapObject::HashTable(_)
            | DumpHeapObject::ByteCode(_)
            | DumpHeapObject::Subr { .. }
            | DumpHeapObject::Buffer(_)
            | DumpHeapObject::Window(_)
            | DumpHeapObject::Frame(_)
            | DumpHeapObject::Timer(_)
            | DumpHeapObject::Free,
        ) => LoadedObjectSpan::Unmapped,
        _ => LoadedObjectSpan::None,
    }
}

pub(crate) fn load_object_starts(section: &[u8]) -> Result<LoadedSpans<'_>, DumpError> {
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
    let count = usize::try_from(header.object_count).map_err(|_| {
        DumpError::ImageFormatError("object-starts object count overflows usize".into())
    })?;
    let mut cursor = HEADER_SIZE;
    let mut offsets = Vec::with_capacity(count);
    for _ in 0..count {
        let offset = u32::try_from(cursor).map_err(|_| {
            DumpError::ImageFormatError("object-starts record offset overflows u32".into())
        })?;
        offsets.push(offset);
        skip_span_record(section, &mut cursor)?;
    }

    Ok(LoadedSpans {
        records: LoadedSpanRecords::Mapped {
            bytes: &section[..cursor],
            offsets,
        },
    })
}

fn skip_span_record(data: &[u8], cursor: &mut usize) -> Result<(), DumpError> {
    if *cursor >= data.len() {
        return Err(DumpError::ImageFormatError(
            "object-starts section truncated".into(),
        ));
    }
    let tag = data[*cursor];
    *cursor += 1;
    match tag {
        SPAN_NONE | SPAN_UNMAPPED => Ok(()),
        SPAN_CONS | SPAN_FLOAT => {
            read_dump_off(data, cursor)?;
            Ok(())
        }
        SPAN_STRING => {
            read_dump_off(data, cursor)?;
            read_dump_off(data, cursor)?;
            Ok(())
        }
        SPAN_VECTORLIKE => {
            read_dump_off(data, cursor)?;
            read_dump_off(data, cursor)?;
            if *cursor >= data.len() {
                return Err(DumpError::ImageFormatError(
                    "object-starts vectorlike slot flag truncated".into(),
                ));
            }
            let has_slots = data[*cursor];
            *cursor += 1;
            if has_slots > 1 {
                return Err(DumpError::ImageFormatError(
                    "object-starts vectorlike slot flag is invalid".into(),
                ));
            }
            if has_slots != 0 {
                read_dump_off(data, cursor)?;
                read_dump_off(data, cursor)?;
            }
            Ok(())
        }
        other => Err(DumpError::ImageFormatError(format!(
            "unknown object-starts span tag {other}"
        ))),
    }
}

fn loaded_span_at(data: &[u8], cursor: usize) -> Result<LoadedObjectSpan, DumpError> {
    if cursor >= data.len() {
        return Err(DumpError::ImageFormatError(
            "object-starts record offset out of range".into(),
        ));
    }
    let tag = data[cursor];
    let mut cursor = cursor + 1;
    match tag {
        SPAN_NONE => Ok(LoadedObjectSpan::None),
        SPAN_UNMAPPED => Ok(LoadedObjectSpan::Unmapped),
        SPAN_CONS => Ok(LoadedObjectSpan::Cons(DumpConsSpan {
            offset: read_dump_off(data, &mut cursor)?,
        })),
        SPAN_FLOAT => Ok(LoadedObjectSpan::Float(DumpFloatSpan {
            offset: read_dump_off(data, &mut cursor)?,
        })),
        SPAN_STRING => Ok(LoadedObjectSpan::String(DumpStringSpan {
            offset: read_dump_off(data, &mut cursor)?,
            len: read_dump_off(data, &mut cursor)?,
        })),
        SPAN_VECTORLIKE => {
            let object = DumpVecLikeSpan {
                offset: read_dump_off(data, &mut cursor)?,
                len: read_dump_off(data, &mut cursor)?,
            };
            if cursor >= data.len() {
                return Err(DumpError::ImageFormatError(
                    "object-starts vectorlike slot flag truncated".into(),
                ));
            }
            let has_slots = data[cursor];
            cursor += 1;
            let slots = if has_slots != 0 {
                Some(DumpSlotSpan {
                    offset: read_dump_off(data, &mut cursor)?,
                    len: read_dump_off(data, &mut cursor)?,
                })
            } else {
                None
            };
            Ok(LoadedObjectSpan::Vectorlike { object, slots })
        }
        other => Err(DumpError::ImageFormatError(format!(
            "unknown object-starts span tag {other}"
        ))),
    }
}

fn write_dump_off(out: &mut Vec<u8>, value: u64) -> Result<(), DumpError> {
    let value = u32::try_from(value).map_err(|_| {
        DumpError::SerializationError(format!("object-starts dump offset {value} overflows u32"))
    })?;
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_dump_off(data: &[u8], cursor: &mut usize) -> Result<u64, DumpError> {
    let end = (*cursor)
        .checked_add(4)
        .ok_or_else(|| DumpError::ImageFormatError("object-starts u32 cursor overflow".into()))?;
    if end > data.len() {
        return Err(DumpError::ImageFormatError(
            "object-starts section truncated at u32".into(),
        ));
    }
    let value = unsafe { std::ptr::read_unaligned(data.as_ptr().add(*cursor).cast::<u32>()) };
    *cursor = end;
    Ok(u32::from_le(value).into())
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
        assert_eq!(spans.len(), 5);
        assert_eq!(spans.cons(0), Some(DumpConsSpan { offset: 0 }));
        assert!(spans.cons(1).is_none());
        assert_eq!(spans.float(1), Some(DumpFloatSpan { offset: 32 }));
        assert_eq!(
            spans.string(4),
            Some(DumpStringSpan {
                offset: 48,
                len: 16
            })
        );
        assert_eq!(
            spans.vectorlike(3),
            Some(DumpVecLikeSpan {
                offset: 64,
                len: 24
            })
        );
        assert_eq!(
            spans.slots(3),
            Some(DumpSlotSpan {
                offset: 88,
                len: 16
            })
        );
        assert_eq!(spans.get(2), LoadedObjectSpan::Unmapped);
    }
}
