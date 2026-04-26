//! Mapped heap payload extraction for pdump images.
//!
//! GNU pdumper keeps object headers in the mapped image and writes cold string
//! data later, then fixes string data pointers to the mapped cold region during
//! load.  Neomacs still reconstructs Rust object headers during this migration,
//! but heap string bytes are already moved out of the serialized runtime-state
//! payload and into the mmap-backed heap section.

use super::DumpError;
use super::types::{DumpByteData, DumpContextState, DumpHeapObject, DumpTaggedHeap};

const HEAP_PAYLOAD_ALIGN: usize = 8;

#[derive(Clone, Copy)]
pub(crate) struct MappedHeapView {
    ptr: *const u8,
    len: usize,
}

pub(crate) struct MappedBytes {
    pub ptr: *const u8,
    pub len: usize,
}

impl MappedHeapView {
    pub(crate) fn from_slice(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
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
                    unsafe { self.ptr.add(start) }
                } else {
                    std::ptr::NonNull::<u8>::dangling().as_ptr()
                };
                Ok(MappedBytes { ptr, len })
            }
        }
    }
}

pub(crate) fn extract_mapped_heap_payloads(state: &mut DumpContextState) -> Vec<u8> {
    extract_tagged_heap_payloads(&mut state.tagged_heap)
}

fn extract_tagged_heap_payloads(heap: &mut DumpTaggedHeap) -> Vec<u8> {
    let mut builder = MappedHeapBuilder::default();

    for object in &mut heap.objects {
        if let DumpHeapObject::Str { data, .. } = object
            && let DumpByteData::Owned(bytes) = data
        {
            let owned = std::mem::take(bytes);
            let span = builder.push_bytes(&owned);
            *data = DumpByteData::mapped(span.offset, span.len);
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
}
