//! Mapped heap payload extraction for pdump images.
//!
//! GNU pdumper keeps object headers in the mapped image and writes cold string
//! data later, then fixes string data pointers to the mapped cold region during
//! load.  Neomacs still reconstructs Rust object headers during this migration,
//! but heap string bytes and vectorlike slot arrays are already moved into the
//! mmap-backed heap section.

use super::DumpError;
use super::types::{DumpByteData, DumpContextState, DumpHeapObject, DumpSlotSpan, DumpTaggedHeap};
use crate::tagged::value::TaggedValue;

const HEAP_PAYLOAD_ALIGN: usize = 8;

#[derive(Clone, Copy)]
pub(crate) struct MappedHeapView {
    ptr: *mut u8,
    len: usize,
    writable: bool,
}

pub(crate) struct MappedBytes {
    pub ptr: *const u8,
    pub len: usize,
}

impl MappedHeapView {
    pub(crate) fn from_slice(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr().cast_mut(),
            len: bytes.len(),
            writable: false,
        }
    }

    pub(crate) fn from_mut_slice(bytes: &mut [u8]) -> Self {
        Self {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            writable: true,
        }
    }

    pub(crate) fn bytes(self, data: &DumpByteData) -> Result<MappedBytes, DumpError> {
        match data {
            DumpByteData::Owned(_) => Err(DumpError::ImageFormatError(
                "owned byte payload requested from mapped heap view".to_string(),
            )),
            DumpByteData::Mapped(span) => {
                let start = usize::try_from(span.offset).map_err(|_| {
                    DumpError::ImageFormatError("mapped heap offset overflows usize".into())
                })?;
                let len = usize::try_from(span.len).map_err(|_| {
                    DumpError::ImageFormatError("mapped heap length overflows usize".into())
                })?;
                let end = start.checked_add(len).ok_or_else(|| {
                    DumpError::ImageFormatError("mapped heap range overflow".into())
                })?;
                if end > self.len {
                    return Err(DumpError::ImageFormatError(format!(
                        "mapped heap range {start}..{end} exceeds heap section length {}",
                        self.len
                    )));
                }
                let ptr = if start < self.len {
                    unsafe { self.ptr.add(start).cast_const() }
                } else {
                    std::ptr::NonNull::<u8>::dangling().as_ptr()
                };
                Ok(MappedBytes { ptr, len })
            }
        }
    }

    pub(crate) fn slots_mut(
        self,
        span: DumpSlotSpan,
        expected_len: usize,
    ) -> Result<*mut TaggedValue, DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let slot_len = usize::try_from(span.len).map_err(|_| {
            DumpError::ImageFormatError("mapped slot span length overflows usize".into())
        })?;
        if slot_len != expected_len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped slot span length {slot_len} does not match vector length {expected_len}"
            )));
        }
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError("mapped slot span offset overflows usize".into())
        })?;
        let byte_len = slot_len
            .checked_mul(std::mem::size_of::<TaggedValue>())
            .ok_or_else(|| {
                DumpError::ImageFormatError("mapped slot byte length overflow".into())
            })?;
        let end = start
            .checked_add(byte_len)
            .ok_or_else(|| DumpError::ImageFormatError("mapped slot span range overflow".into()))?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped slot span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<TaggedValue>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped slot span offset {start} is not {}-byte aligned",
                std::mem::align_of::<TaggedValue>()
            )));
        }
        if slot_len == 0 {
            Ok(std::ptr::NonNull::<TaggedValue>::dangling().as_ptr())
        } else {
            Ok(unsafe { self.ptr.add(start).cast::<TaggedValue>() })
        }
    }
}

pub(crate) fn extract_mapped_heap_payloads(state: &mut DumpContextState) -> Vec<u8> {
    extract_tagged_heap_payloads(&mut state.tagged_heap)
}

fn extract_tagged_heap_payloads(heap: &mut DumpTaggedHeap) -> Vec<u8> {
    let mut builder = MappedHeapBuilder::default();

    heap.mapped_slots.clear();
    heap.mapped_slots.resize(heap.objects.len(), None);

    for (index, object) in heap.objects.iter_mut().enumerate() {
        if let DumpHeapObject::Str { data, .. } = object
            && let DumpByteData::Owned(bytes) = data
        {
            let owned = std::mem::take(bytes);
            let span = builder.push_bytes(&owned);
            *data = DumpByteData::mapped(span.offset, span.len);
        }

        let slot_count = match object {
            DumpHeapObject::Vector(slots)
            | DumpHeapObject::Lambda(slots)
            | DumpHeapObject::Macro(slots)
            | DumpHeapObject::Record(slots) => Some(slots.len()),
            _ => None,
        };
        if let Some(slot_count) = slot_count {
            heap.mapped_slots[index] = Some(builder.reserve_slots(slot_count));
        }
    }

    builder.finish()
}

#[derive(Default)]
struct MappedHeapBuilder {
    bytes: Vec<u8>,
}

impl MappedHeapBuilder {
    fn push_bytes(&mut self, payload: &[u8]) -> super::types::DumpByteSpan {
        let padding = align_padding(self.bytes.len(), HEAP_PAYLOAD_ALIGN);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        if payload.is_empty() {
            self.bytes.push(0);
            return super::types::DumpByteSpan {
                offset: offset as u64,
                len: 0,
            };
        }
        self.bytes.extend_from_slice(payload);
        super::types::DumpByteSpan {
            offset: offset as u64,
            len: payload.len() as u64,
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn reserve_slots(&mut self, slot_count: usize) -> DumpSlotSpan {
        let align = std::mem::align_of::<TaggedValue>().max(HEAP_PAYLOAD_ALIGN);
        let padding = align_padding(self.bytes.len(), align);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        let byte_len = slot_count.saturating_mul(std::mem::size_of::<TaggedValue>());
        if byte_len == 0 {
            self.bytes
                .resize(self.bytes.len() + std::mem::size_of::<TaggedValue>(), 0);
        } else {
            self.bytes.resize(self.bytes.len() + byte_len, 0);
        }
        DumpSlotSpan {
            offset: offset as u64,
            len: slot_count as u64,
        }
    }
}

fn align_padding(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (align - (value & (align - 1))) & (align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::pdump::types::DumpTaggedHeap;

    #[test]
    fn extracts_string_bytes_into_mapped_heap_section() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Str {
                data: DumpByteData::owned(b"abc".to_vec()),
                size: 3,
                size_byte: 3,
                text_props: Vec::new(),
            }],
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(heap, b"abc");
        let DumpHeapObject::Str { data, .. } = &tagged_heap.objects[0] else {
            panic!("expected string object");
        };
        assert_eq!(*data, DumpByteData::mapped(0, 3));

        let view = MappedHeapView::from_slice(&heap);
        let mapped = view.bytes(data).unwrap();
        let mapped_bytes = unsafe { std::slice::from_raw_parts(mapped.ptr, mapped.len) };
        assert_eq!(mapped_bytes, b"abc");
    }

    #[test]
    fn empty_strings_still_create_heap_section_anchor() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Str {
                data: DumpByteData::owned(Vec::new()),
                size: 0,
                size_byte: 0,
                text_props: Vec::new(),
            }],
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(heap, [0]);
        let DumpHeapObject::Str { data, .. } = &tagged_heap.objects[0] else {
            panic!("expected string object");
        };
        assert_eq!(*data, DumpByteData::mapped(0, 0));
        let view = MappedHeapView::from_slice(&heap);
        let mapped = view.bytes(data).unwrap();
        assert_eq!(mapped.len, 0);
        assert_eq!(mapped.ptr, heap.as_ptr());
    }

    #[test]
    fn reserves_aligned_slot_spans_for_vectorlike_objects() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Vector(vec![
                crate::emacs_core::pdump::types::DumpValue::Int(1),
                crate::emacs_core::pdump::types::DumpValue::Int(2),
            ])],
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(
            heap.len(),
            2 * std::mem::size_of::<crate::tagged::value::TaggedValue>()
        );
        assert_eq!(tagged_heap.mapped_slots.len(), 1);
        let span = tagged_heap.mapped_slots[0].expect("vector slot span");
        assert_eq!(span.offset, 0);
        assert_eq!(span.len, 2);
    }
}
