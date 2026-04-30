//! Mapped heap payload extraction for pdump images.
//!
//! GNU pdumper keeps object headers in the mapped image and writes cold string
//! data later, then fixes string data pointers to the mapped cold region during
//! load.  Neomacs is migrating heap classes onto that same shape: mapped object
//! headers, mapped string bytes, mapped vectorlike slot arrays, and external GC
//! mark bits.

use super::DumpError;
use super::mmap_image::ImageRelocation;
use super::types::{
    DumpByteData, DumpConsSpan, DumpContextState, DumpFloatSpan, DumpHeapObject, DumpSlotSpan,
    DumpStringSpan, DumpTaggedHeap, DumpValue, DumpVecLikeSpan,
};
use super::value_fixups::RawValueFixup;
use crate::heap_types::LispString;
use crate::tagged::header::{
    ConsCell, FloatObj, GcHeader, HeapObjectKind, LambdaObj, MacroObj, MarkerObj, OverlayObj,
    RecordObj, StringObj, VecLikeHeader, VecLikeType, VectorObj,
};
use crate::tagged::value::TaggedValue;
use bytemuck::{Pod, Zeroable};

const HEAP_PAYLOAD_ALIGN: usize = 8;
const TAG_CONS: u64 = 0b011;
const TAG_STRING: u64 = 0b100;
const TAG_VECLIKE: u64 = 0b101;
const TAG_FLOAT: u64 = 0b111;

#[derive(Default)]
pub(crate) struct MappedHeapPayload {
    pub bytes: Vec<u8>,
    pub relocations: Vec<ImageRelocation>,
    pub value_fixups: Vec<RawValueFixup>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RawGcHeader {
    marked: u8,
    kind: u8,
    padding: [u8; 6],
    next: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RawFloatObj {
    header: RawGcHeader,
    value: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RawVecLikeHeader {
    header: RawGcHeader,
    type_tag: u8,
    padding: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RawStringObj {
    header: RawGcHeader,
    size: usize,
    size_byte: i64,
    intervals: usize,
    data: usize,
    storage: usize,
}

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

    pub(crate) unsafe fn from_raw_parts(ptr: *mut u8, len: usize, writable: bool) -> Self {
        Self { ptr, len, writable }
    }

    pub(crate) fn bytes(self, data: &DumpByteData) -> Result<MappedBytes, DumpError> {
        match data {
            DumpByteData::Owned(_) | DumpByteData::StaticRoData { .. } => {
                Err(DumpError::ImageFormatError(
                    "owned byte payload requested from mapped heap view".to_string(),
                ))
            }
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
                let terminator_end = end.checked_add(1).ok_or_else(|| {
                    DumpError::ImageFormatError("mapped heap terminator range overflow".into())
                })?;
                if terminator_end > self.len {
                    return Err(DumpError::ImageFormatError(format!(
                        "mapped heap range {start}..{terminator_end} exceeds heap section length {}",
                        self.len
                    )));
                }
                let terminator = unsafe { *self.ptr.add(end) };
                if terminator != 0 {
                    return Err(DumpError::ImageFormatError(format!(
                        "mapped heap string data at {start}..{end} is missing GNU trailing NUL"
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

    pub(crate) fn cons_cell_mut(self, span: DumpConsSpan) -> Result<*mut ConsCell, DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError("mapped cons span offset overflows usize".into())
        })?;
        let end = start
            .checked_add(std::mem::size_of::<ConsCell>())
            .ok_or_else(|| DumpError::ImageFormatError("mapped cons span range overflow".into()))?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped cons span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<ConsCell>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped cons span offset {start} is not {}-byte aligned",
                std::mem::align_of::<ConsCell>()
            )));
        }
        Ok(unsafe { self.ptr.add(start).cast::<ConsCell>() })
    }

    pub(crate) fn float_obj_mut(self, span: DumpFloatSpan) -> Result<*mut FloatObj, DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError("mapped float span offset overflows usize".into())
        })?;
        let end = start
            .checked_add(std::mem::size_of::<FloatObj>())
            .ok_or_else(|| {
                DumpError::ImageFormatError("mapped float span range overflow".into())
            })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped float span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<FloatObj>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped float span offset {start} is not {}-byte aligned",
                std::mem::align_of::<FloatObj>()
            )));
        }
        Ok(unsafe { self.ptr.add(start).cast::<FloatObj>() })
    }

    pub(crate) fn typed_object_mut<T>(
        self,
        span: DumpVecLikeSpan,
        label: &'static str,
    ) -> Result<*mut T, DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError(format!("mapped {label} span offset overflows usize"))
        })?;
        let len = usize::try_from(span.len).map_err(|_| {
            DumpError::ImageFormatError(format!("mapped {label} span length overflows usize"))
        })?;
        let expected = std::mem::size_of::<T>();
        if len != expected {
            return Err(DumpError::ImageFormatError(format!(
                "mapped {label} span length {len} does not match object size {expected}"
            )));
        }
        let end = start.checked_add(len).ok_or_else(|| {
            DumpError::ImageFormatError(format!("mapped {label} span range overflow"))
        })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped {label} span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<T>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped {label} span offset {start} is not {}-byte aligned",
                std::mem::align_of::<T>()
            )));
        }
        Ok(unsafe { self.ptr.add(start).cast::<T>() })
    }

    pub(crate) fn veclike_header_mut(
        self,
        span: DumpVecLikeSpan,
    ) -> Result<*mut VecLikeHeader, DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError("mapped vectorlike span offset overflows usize".into())
        })?;
        let len = usize::try_from(span.len).map_err(|_| {
            DumpError::ImageFormatError("mapped vectorlike span length overflows usize".into())
        })?;
        if len < std::mem::size_of::<VecLikeHeader>() {
            return Err(DumpError::ImageFormatError(format!(
                "mapped vectorlike span length {len} is smaller than header size {}",
                std::mem::size_of::<VecLikeHeader>()
            )));
        }
        let end = start.checked_add(len).ok_or_else(|| {
            DumpError::ImageFormatError("mapped vectorlike span range overflow".into())
        })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped vectorlike span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<VecLikeHeader>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped vectorlike span offset {start} is not {}-byte aligned",
                std::mem::align_of::<VecLikeHeader>()
            )));
        }
        Ok(unsafe { self.ptr.add(start).cast::<VecLikeHeader>() })
    }

    pub(crate) fn veclike_type(self, span: DumpVecLikeSpan) -> Result<VecLikeType, DumpError> {
        let start = usize::try_from(span.offset).map_err(|_| {
            DumpError::ImageFormatError("mapped vectorlike span offset overflows usize".into())
        })?;
        let len = usize::try_from(span.len).map_err(|_| {
            DumpError::ImageFormatError("mapped vectorlike span length overflows usize".into())
        })?;
        if len < std::mem::size_of::<VecLikeHeader>() {
            return Err(DumpError::ImageFormatError(format!(
                "mapped vectorlike span length {len} is smaller than header size {}",
                std::mem::size_of::<VecLikeHeader>()
            )));
        }
        let end = start.checked_add(len).ok_or_else(|| {
            DumpError::ImageFormatError("mapped vectorlike span range overflow".into())
        })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped vectorlike span {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        let tag_offset = start + std::mem::size_of::<GcHeader>();
        let tag = unsafe { *self.ptr.add(tag_offset) };
        veclike_type_from_tag(tag)
    }

    pub(crate) fn string_obj_mut(self, span: DumpStringSpan) -> Result<*mut StringObj, DumpError> {
        self.typed_object_mut::<StringObj>(
            DumpVecLikeSpan {
                offset: span.offset,
                len: span.len,
            },
            "string",
        )
    }

    pub(crate) fn write_value_word(self, offset: u64, value: TaggedValue) -> Result<(), DumpError> {
        if !self.writable {
            return Err(DumpError::ImageFormatError(
                "mapped heap view is not writable".to_string(),
            ));
        }
        let start = usize::try_from(offset).map_err(|_| {
            DumpError::ImageFormatError("mapped value fixup offset overflows usize".into())
        })?;
        let end = start
            .checked_add(std::mem::size_of::<TaggedValue>())
            .ok_or_else(|| {
                DumpError::ImageFormatError("mapped value fixup range overflow".into())
            })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped value fixup {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<TaggedValue>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped value fixup offset {start} is not {}-byte aligned",
                std::mem::align_of::<TaggedValue>()
            )));
        }
        unsafe {
            self.ptr
                .add(start)
                .cast::<usize>()
                .write_unaligned(value.bits());
        }
        Ok(())
    }

    pub(crate) fn read_value_word(self, offset: u64) -> Result<usize, DumpError> {
        let start = usize::try_from(offset).map_err(|_| {
            DumpError::ImageFormatError("mapped value fixup offset overflows usize".into())
        })?;
        let end = start
            .checked_add(std::mem::size_of::<TaggedValue>())
            .ok_or_else(|| {
                DumpError::ImageFormatError("mapped value fixup range overflow".into())
            })?;
        if end > self.len {
            return Err(DumpError::ImageFormatError(format!(
                "mapped value fixup {start}..{end} exceeds heap section length {}",
                self.len
            )));
        }
        if start % std::mem::align_of::<TaggedValue>() != 0 {
            return Err(DumpError::ImageFormatError(format!(
                "mapped value fixup offset {start} is not {}-byte aligned",
                std::mem::align_of::<TaggedValue>()
            )));
        }
        Ok(unsafe { self.ptr.add(start).cast::<usize>().read_unaligned() })
    }
}

pub(crate) fn extract_mapped_heap_payloads(state: &mut DumpContextState) -> MappedHeapPayload {
    extract_tagged_heap_payloads(&mut state.tagged_heap)
}

pub(crate) fn clear_heap_metadata(heap: &mut DumpTaggedHeap) {
    heap.mapped_cons.clear();
    heap.mapped_floats.clear();
    heap.mapped_strings.clear();
    heap.mapped_veclikes.clear();
    heap.mapped_slots.clear();
}

pub(crate) fn rebuild_heap_metadata(heap: &mut DumpTaggedHeap) -> Result<(), DumpError> {
    let mut layout = HeapLayoutCursor::default();

    heap.mapped_cons.clear();
    heap.mapped_cons.resize(heap.objects.len(), None);
    heap.mapped_floats.clear();
    heap.mapped_floats.resize(heap.objects.len(), None);
    heap.mapped_strings.clear();
    heap.mapped_strings.resize(heap.objects.len(), None);
    heap.mapped_veclikes.clear();
    heap.mapped_veclikes.resize(heap.objects.len(), None);
    heap.mapped_slots.clear();
    heap.mapped_slots.resize(heap.objects.len(), None);

    let cons_count = heap
        .objects
        .iter()
        .filter(|object| matches!(object, DumpHeapObject::Cons { .. }))
        .count();
    let cons_base = layout.reserve_cons_cells(cons_count);
    let mut cons_index = 0usize;
    let float_count = heap
        .objects
        .iter()
        .filter(|object| matches!(object, DumpHeapObject::Float(_)))
        .count();
    let float_base = layout.reserve_float_objects(float_count);
    let mut float_index = 0usize;

    for (index, object) in heap.objects.iter().enumerate() {
        if matches!(object, DumpHeapObject::Cons { .. }) {
            let offset = cons_base.expect("non-zero cons count should reserve a mapped cons arena")
                + cons_index * std::mem::size_of::<ConsCell>();
            heap.mapped_cons[index] = Some(DumpConsSpan {
                offset: offset as u64,
            });
            cons_index += 1;
        }

        if matches!(object, DumpHeapObject::Float(_)) {
            let offset = float_base.expect("non-zero float count should reserve mapped floats")
                + float_index * std::mem::size_of::<FloatObj>();
            heap.mapped_floats[index] = Some(DumpFloatSpan {
                offset: offset as u64,
            });
            float_index += 1;
        }

        match object {
            DumpHeapObject::Vector(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<VectorObj>());
            }
            DumpHeapObject::Lambda(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<LambdaObj>());
            }
            DumpHeapObject::Macro(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<MacroObj>());
            }
            DumpHeapObject::Record(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<RecordObj>());
            }
            DumpHeapObject::Marker(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<MarkerObj>());
            }
            DumpHeapObject::Overlay(_) => {
                heap.mapped_veclikes[index] = Some(layout.reserve_typed_object::<OverlayObj>());
            }
            _ => {}
        }

        if let DumpHeapObject::Str { data, .. } = object {
            let span = layout.reserve_typed_object::<StringObj>();
            heap.mapped_strings[index] = Some(DumpStringSpan {
                offset: span.offset,
                len: span.len,
            });
            match data {
                DumpByteData::Owned(bytes) => {
                    layout.push_bytes_len(bytes.len());
                }
                DumpByteData::Mapped(span) => {
                    let rebuilt = layout.push_bytes_len(span.len as usize);
                    if rebuilt != *span {
                        return Err(DumpError::ImageFormatError(format!(
                            "mapped string data span mismatch for heap object {index}: dump has {span:?}, rebuilt {rebuilt:?}"
                        )));
                    }
                }
                DumpByteData::StaticRoData { .. } => {}
            }
        }

        let slot_count = match object {
            DumpHeapObject::Vector(slots)
            | DumpHeapObject::Lambda(slots)
            | DumpHeapObject::Macro(slots)
            | DumpHeapObject::Record(slots) => Some(slots.len()),
            _ => None,
        };
        if let Some(slot_count) = slot_count {
            heap.mapped_slots[index] = Some(layout.reserve_slots(slot_count));
        }
    }

    Ok(())
}

fn extract_tagged_heap_payloads(heap: &mut DumpTaggedHeap) -> MappedHeapPayload {
    let mut builder = MappedHeapBuilder::default();

    heap.mapped_cons.clear();
    heap.mapped_cons.resize(heap.objects.len(), None);
    heap.mapped_floats.clear();
    heap.mapped_floats.resize(heap.objects.len(), None);
    heap.mapped_strings.clear();
    heap.mapped_strings.resize(heap.objects.len(), None);
    heap.mapped_veclikes.clear();
    heap.mapped_veclikes.resize(heap.objects.len(), None);
    heap.mapped_slots.clear();
    heap.mapped_slots.resize(heap.objects.len(), None);

    let cons_count = heap
        .objects
        .iter()
        .filter(|object| matches!(object, DumpHeapObject::Cons { .. }))
        .count();
    let cons_base = builder.reserve_cons_cells(cons_count);
    let mut cons_index = 0usize;
    let float_count = heap
        .objects
        .iter()
        .filter(|object| matches!(object, DumpHeapObject::Float(_)))
        .count();
    let float_base = builder.reserve_float_objects(float_count);
    let mut float_index = 0usize;

    for (index, object) in heap.objects.iter_mut().enumerate() {
        if matches!(object, DumpHeapObject::Cons { .. }) {
            let offset = cons_base.expect("non-zero cons count should reserve a mapped cons arena")
                + cons_index * std::mem::size_of::<ConsCell>();
            heap.mapped_cons[index] = Some(DumpConsSpan {
                offset: offset as u64,
            });
            cons_index += 1;
        }

        if matches!(object, DumpHeapObject::Float(_)) {
            let offset = float_base.expect("non-zero float count should reserve mapped floats")
                + float_index * std::mem::size_of::<FloatObj>();
            heap.mapped_floats[index] = Some(DumpFloatSpan {
                offset: offset as u64,
            });
            float_index += 1;
        }

        match object {
            DumpHeapObject::Vector(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<VectorObj>());
            }
            DumpHeapObject::Lambda(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<LambdaObj>());
            }
            DumpHeapObject::Macro(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<MacroObj>());
            }
            DumpHeapObject::Record(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<RecordObj>());
            }
            DumpHeapObject::Marker(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<MarkerObj>());
            }
            DumpHeapObject::Overlay(_) => {
                heap.mapped_veclikes[index] = Some(builder.reserve_typed_object::<OverlayObj>());
            }
            _ => {}
        }

        if let DumpHeapObject::Str { data, .. } = object {
            let span = builder.reserve_typed_object::<StringObj>();
            heap.mapped_strings[index] = Some(DumpStringSpan {
                offset: span.offset,
                len: span.len,
            });
            match data {
                DumpByteData::Owned(bytes) => {
                    let owned = std::mem::take(bytes);
                    let span = builder.push_bytes(&owned);
                    *data = DumpByteData::mapped(span.offset, span.len);
                }
                DumpByteData::Mapped(_) | DumpByteData::StaticRoData { .. } => {}
            }
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

    builder.populate_raw_heap_payloads(heap);
    builder.finish()
}

#[derive(Default)]
struct HeapLayoutCursor {
    offset: usize,
}

impl HeapLayoutCursor {
    fn push_bytes_len(&mut self, payload_len: usize) -> super::types::DumpByteSpan {
        self.align_to(HEAP_PAYLOAD_ALIGN);
        let offset = self.offset;
        self.offset += payload_len + 1;
        super::types::DumpByteSpan {
            offset: offset as u64,
            len: payload_len as u64,
        }
    }

    fn reserve_slots(&mut self, slot_count: usize) -> DumpSlotSpan {
        let align = std::mem::align_of::<TaggedValue>().max(HEAP_PAYLOAD_ALIGN);
        self.align_to(align);
        let offset = self.offset;
        let byte_len = slot_count.saturating_mul(std::mem::size_of::<TaggedValue>());
        if byte_len == 0 {
            self.offset += std::mem::size_of::<TaggedValue>();
        } else {
            self.offset += byte_len;
        }
        DumpSlotSpan {
            offset: offset as u64,
            len: slot_count as u64,
        }
    }

    fn reserve_cons_cells(&mut self, cons_count: usize) -> Option<usize> {
        if cons_count == 0 {
            return None;
        }
        let align = std::mem::align_of::<ConsCell>().max(HEAP_PAYLOAD_ALIGN);
        self.align_to(align);
        let offset = self.offset;
        self.offset += cons_count * std::mem::size_of::<ConsCell>();
        Some(offset)
    }

    fn reserve_float_objects(&mut self, float_count: usize) -> Option<usize> {
        if float_count == 0 {
            return None;
        }
        let align = std::mem::align_of::<FloatObj>().max(HEAP_PAYLOAD_ALIGN);
        self.align_to(align);
        let offset = self.offset;
        self.offset += float_count * std::mem::size_of::<FloatObj>();
        Some(offset)
    }

    fn reserve_typed_object<T>(&mut self) -> DumpVecLikeSpan {
        let align = std::mem::align_of::<T>().max(HEAP_PAYLOAD_ALIGN);
        self.align_to(align);
        let offset = self.offset;
        let len = std::mem::size_of::<T>();
        self.offset += len;
        DumpVecLikeSpan {
            offset: offset as u64,
            len: len as u64,
        }
    }

    fn align_to(&mut self, align: usize) {
        self.offset += align_padding(self.offset, align);
    }
}

#[derive(Default)]
struct MappedHeapBuilder {
    bytes: Vec<u8>,
    relocations: Vec<ImageRelocation>,
    value_fixups: Vec<RawValueFixup>,
}

impl MappedHeapBuilder {
    fn push_bytes(&mut self, payload: &[u8]) -> super::types::DumpByteSpan {
        let padding = align_padding(self.bytes.len(), HEAP_PAYLOAD_ALIGN);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(payload);
        self.bytes.push(0);
        super::types::DumpByteSpan {
            offset: offset as u64,
            len: payload.len() as u64,
        }
    }

    fn finish(self) -> MappedHeapPayload {
        MappedHeapPayload {
            bytes: self.bytes,
            relocations: self.relocations,
            value_fixups: self.value_fixups,
        }
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

    fn reserve_cons_cells(&mut self, cons_count: usize) -> Option<usize> {
        if cons_count == 0 {
            return None;
        }
        let align = std::mem::align_of::<ConsCell>().max(HEAP_PAYLOAD_ALIGN);
        let padding = align_padding(self.bytes.len(), align);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        self.bytes
            .resize(offset + cons_count * std::mem::size_of::<ConsCell>(), 0);
        Some(offset)
    }

    fn reserve_float_objects(&mut self, float_count: usize) -> Option<usize> {
        if float_count == 0 {
            return None;
        }
        let align = std::mem::align_of::<FloatObj>().max(HEAP_PAYLOAD_ALIGN);
        let padding = align_padding(self.bytes.len(), align);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        self.bytes
            .resize(offset + float_count * std::mem::size_of::<FloatObj>(), 0);
        Some(offset)
    }

    fn reserve_typed_object<T>(&mut self) -> DumpVecLikeSpan {
        let align = std::mem::align_of::<T>().max(HEAP_PAYLOAD_ALIGN);
        let padding = align_padding(self.bytes.len(), align);
        self.bytes.resize(self.bytes.len() + padding, 0);
        let offset = self.bytes.len();
        let len = std::mem::size_of::<T>();
        self.bytes.resize(offset + len, 0);
        DumpVecLikeSpan {
            offset: offset as u64,
            len: len as u64,
        }
    }

    fn populate_raw_heap_payloads(&mut self, heap: &DumpTaggedHeap) {
        self.debug_assert_raw_layout_matches_runtime();
        for (index, object) in heap.objects.iter().enumerate() {
            match object {
                DumpHeapObject::Cons { car, cdr } => {
                    if let Some(span) = heap.mapped_cons.get(index).copied().flatten() {
                        let offset = span.offset as usize;
                        self.write_dump_value_word(offset, car, heap);
                        self.write_dump_value_word(
                            offset + std::mem::size_of::<TaggedValue>(),
                            cdr,
                            heap,
                        );
                    }
                }
                DumpHeapObject::Float(value) => {
                    if let Some(span) = heap.mapped_floats.get(index).copied().flatten() {
                        self.write_raw_float_obj(span.offset as usize, *value);
                    }
                }
                DumpHeapObject::Vector(slots)
                | DumpHeapObject::Lambda(slots)
                | DumpHeapObject::Macro(slots)
                | DumpHeapObject::Record(slots) => {
                    if let Some(span) = heap.mapped_veclikes.get(index).copied().flatten() {
                        let type_tag = match object {
                            DumpHeapObject::Vector(_) => VecLikeType::Vector,
                            DumpHeapObject::Lambda(_) => VecLikeType::Lambda,
                            DumpHeapObject::Macro(_) => VecLikeType::Macro,
                            DumpHeapObject::Record(_) => VecLikeType::Record,
                            _ => unreachable!(),
                        };
                        self.write_raw_veclike_header(span.offset as usize, type_tag);
                    }
                    if let Some(span) = heap.mapped_slots.get(index).copied().flatten() {
                        let mut offset = span.offset as usize;
                        for slot in slots {
                            self.write_dump_value_word(offset, slot, heap);
                            offset += std::mem::size_of::<TaggedValue>();
                        }
                    }
                }
                DumpHeapObject::Marker(_) | DumpHeapObject::Overlay(_) => {
                    if let Some(span) = heap.mapped_veclikes.get(index).copied().flatten() {
                        let type_tag = match object {
                            DumpHeapObject::Marker(_) => VecLikeType::Marker,
                            DumpHeapObject::Overlay(_) => VecLikeType::Overlay,
                            _ => unreachable!(),
                        };
                        self.write_raw_veclike_header(span.offset as usize, type_tag);
                    }
                }
                DumpHeapObject::Str {
                    data,
                    size,
                    size_byte,
                    ..
                } => {
                    if let Some(span) = heap.mapped_strings.get(index).copied().flatten() {
                        self.write_raw_string_obj(span.offset as usize, *size, *size_byte, data);
                    }
                }
                _ => {}
            }
        }
    }

    fn debug_assert_raw_layout_matches_runtime(&self) {
        debug_assert_eq!(
            std::mem::size_of::<RawGcHeader>(),
            std::mem::size_of::<GcHeader>()
        );
        debug_assert_eq!(
            std::mem::align_of::<RawGcHeader>(),
            std::mem::align_of::<GcHeader>()
        );
        debug_assert_eq!(
            std::mem::size_of::<RawFloatObj>(),
            std::mem::size_of::<FloatObj>()
        );
        debug_assert_eq!(
            std::mem::align_of::<RawFloatObj>(),
            std::mem::align_of::<FloatObj>()
        );
        debug_assert_eq!(
            std::mem::size_of::<RawVecLikeHeader>(),
            std::mem::size_of::<VecLikeHeader>()
        );
        debug_assert_eq!(
            std::mem::align_of::<RawVecLikeHeader>(),
            std::mem::align_of::<VecLikeHeader>()
        );
        debug_assert_eq!(
            std::mem::size_of::<RawStringObj>(),
            std::mem::size_of::<StringObj>()
        );
        debug_assert_eq!(
            std::mem::align_of::<RawStringObj>(),
            std::mem::align_of::<StringObj>()
        );
    }

    fn write_raw_float_obj(&mut self, offset: usize, value: f64) {
        let raw = RawFloatObj {
            header: RawGcHeader {
                marked: 0,
                kind: HeapObjectKind::Float as u8,
                padding: [0; 6],
                next: 0,
            },
            value,
        };
        self.write_bytes(offset, bytemuck::bytes_of(&raw));
    }

    fn write_raw_veclike_header(&mut self, offset: usize, type_tag: VecLikeType) {
        let raw = RawVecLikeHeader {
            header: RawGcHeader {
                marked: 0,
                kind: HeapObjectKind::VecLike as u8,
                padding: [0; 6],
                next: 0,
            },
            type_tag: type_tag as u8,
            padding: [0; 7],
        };
        self.write_bytes(offset, bytemuck::bytes_of(&raw));
    }

    fn write_raw_string_obj(
        &mut self,
        offset: usize,
        size: usize,
        size_byte: i64,
        data: &DumpByteData,
    ) {
        let data_word = match data {
            DumpByteData::Mapped(span) => {
                let data_field_offset = offset
                    + std::mem::offset_of!(StringObj, data)
                    + LispString::data_field_offset();
                self.relocations.push(ImageRelocation {
                    location_offset: data_field_offset as u64,
                    addend: 0,
                });
                usize::try_from(span.offset)
                    .expect("mapped string byte offset should fit in a word")
            }
            DumpByteData::StaticRoData { .. } => 0,
            DumpByteData::Owned(_) => {
                unreachable!("owned string bytes should be extracted before raw heap population")
            }
        };
        let raw = RawStringObj {
            header: RawGcHeader {
                marked: 0,
                kind: HeapObjectKind::String as u8,
                padding: [0; 6],
                next: 0,
            },
            size,
            size_byte,
            intervals: 0,
            data: data_word,
            storage: 0,
        };
        self.write_bytes(offset, bytemuck::bytes_of(&raw));
    }

    fn write_dump_value_word(&mut self, offset: usize, value: &DumpValue, heap: &DumpTaggedHeap) {
        let Some(word) = self.dump_value_word(offset as u64, value, heap) else {
            self.value_fixups.push(RawValueFixup::Value {
                location_offset: offset as u64,
                value: value.clone(),
            });
            let word = TaggedValue::NIL.bits();
            self.write_bytes(offset, &word.to_ne_bytes());
            return;
        };
        self.write_bytes(offset, &word.to_ne_bytes());
    }

    fn dump_value_word(
        &mut self,
        location_offset: u64,
        value: &DumpValue,
        heap: &DumpTaggedHeap,
    ) -> Option<usize> {
        match value {
            DumpValue::Nil => Some(TaggedValue::NIL.bits()),
            DumpValue::True => Some(TaggedValue::T.bits()),
            DumpValue::Int(n) => Some(TaggedValue::fixnum(*n).bits()),
            DumpValue::Unbound => Some(TaggedValue::UNBOUND.bits()),
            DumpValue::Symbol(id) => {
                self.value_fixups
                    .push(RawValueFixup::Symbol { location_offset });
                Some(id.0 as usize)
            }
            _ => {
                let (target_offset, tag) = mapped_heap_ref_target(value, heap)?;
                self.relocations.push(ImageRelocation {
                    location_offset,
                    addend: tag as u8,
                });
                Some(
                    usize::try_from(target_offset)
                        .expect("mapped heap relocation target offset should fit in a word"),
                )
            }
        }
    }

    fn write_bytes(&mut self, offset: usize, payload: &[u8]) {
        let end = offset
            .checked_add(payload.len())
            .expect("mapped heap write range should not overflow");
        self.bytes[offset..end].copy_from_slice(payload);
    }
}

fn mapped_heap_ref_target(value: &DumpValue, heap: &DumpTaggedHeap) -> Option<(u64, u64)> {
    match value {
        DumpValue::Cons(id) => heap
            .mapped_cons
            .get(id.index as usize)
            .copied()
            .flatten()
            .map(|span| (span.offset, TAG_CONS)),
        DumpValue::Float(id) => heap
            .mapped_floats
            .get(id.index as usize)
            .copied()
            .flatten()
            .map(|span| (span.offset, TAG_FLOAT)),
        DumpValue::Str(id) => heap
            .mapped_strings
            .get(id.index as usize)
            .copied()
            .flatten()
            .map(|span| (span.offset, TAG_STRING)),
        DumpValue::Vector(id)
        | DumpValue::Record(id)
        | DumpValue::Lambda(id)
        | DumpValue::Macro(id)
        | DumpValue::Marker(id)
        | DumpValue::Overlay(id) => heap
            .mapped_veclikes
            .get(id.index as usize)
            .copied()
            .flatten()
            .map(|span| (span.offset, TAG_VECLIKE)),
        _ => None,
    }
}

fn veclike_type_from_tag(tag: u8) -> Result<VecLikeType, DumpError> {
    match tag {
        0 => Ok(VecLikeType::Vector),
        1 => Ok(VecLikeType::HashTable),
        2 => Ok(VecLikeType::Lambda),
        3 => Ok(VecLikeType::Macro),
        4 => Ok(VecLikeType::ByteCode),
        5 => Ok(VecLikeType::Record),
        6 => Ok(VecLikeType::Overlay),
        7 => Ok(VecLikeType::Marker),
        8 => Ok(VecLikeType::Buffer),
        9 => Ok(VecLikeType::Window),
        10 => Ok(VecLikeType::Frame),
        11 => Ok(VecLikeType::Timer),
        12 => Ok(VecLikeType::Subr),
        13 => Ok(VecLikeType::Bignum),
        14 => Ok(VecLikeType::SymbolWithPos),
        other => Err(DumpError::ImageFormatError(format!(
            "unknown mapped vectorlike type tag {other}"
        ))),
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
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(tagged_heap.mapped_strings.len(), 1);
        let string_span = tagged_heap.mapped_strings[0].expect("string object span");
        assert_eq!(string_span.offset, 0);
        assert_eq!(string_span.len as usize, std::mem::size_of::<StringObj>());
        let DumpHeapObject::Str { data, .. } = &tagged_heap.objects[0] else {
            panic!("expected string object");
        };

        let view = MappedHeapView::from_slice(&heap.bytes);
        let mapped = view.bytes(data).unwrap();
        let mapped_bytes = unsafe { std::slice::from_raw_parts(mapped.ptr, mapped.len) };
        assert_eq!(mapped_bytes, b"abc");
        assert_eq!(unsafe { *mapped.ptr.add(mapped.len) }, 0);

        let object_offset = string_span.offset as usize;
        let data_field_offset = object_offset + std::mem::offset_of!(RawStringObj, data);
        assert_eq!(heap.bytes[object_offset + 1], HeapObjectKind::String as u8);
        assert_eq!(
            read_usize(
                &heap.bytes,
                object_offset + std::mem::offset_of!(RawStringObj, size)
            ),
            3
        );
        assert_eq!(
            read_i64(
                &heap.bytes,
                object_offset + std::mem::offset_of!(RawStringObj, size_byte)
            ),
            3
        );
        assert_eq!(
            read_usize(&heap.bytes, data_field_offset),
            mapped.ptr as usize - heap.bytes.as_ptr() as usize
        );
        assert!(
            heap.relocations
                .iter()
                .any(
                    |relocation| relocation.location_offset == data_field_offset as u64
                        && relocation.addend == 0
                )
        );
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
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert!(heap.bytes.len() > std::mem::size_of::<StringObj>());
        let DumpHeapObject::Str { data, .. } = &tagged_heap.objects[0] else {
            panic!("expected string object");
        };
        let view = MappedHeapView::from_slice(&heap.bytes);
        let mapped = view.bytes(data).unwrap();
        assert_eq!(mapped.len, 0);
        assert!(mapped.ptr as usize >= heap.bytes.as_ptr() as usize);
        assert_eq!(unsafe { *mapped.ptr }, 0);
    }

    #[test]
    fn reserves_aligned_slot_spans_for_vectorlike_objects() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Vector(vec![
                crate::emacs_core::pdump::types::DumpValue::Int(1),
                crate::emacs_core::pdump::types::DumpValue::Int(2),
            ])],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let mut heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert!(heap.bytes.len() >= std::mem::size_of::<VectorObj>());
        assert_eq!(tagged_heap.mapped_veclikes.len(), 1);
        let object_span = tagged_heap.mapped_veclikes[0].expect("vector object span");
        assert_eq!(object_span.offset, 0);
        assert_eq!(object_span.len as usize, std::mem::size_of::<VectorObj>());
        assert_eq!(tagged_heap.mapped_slots.len(), 1);
        let span = tagged_heap.mapped_slots[0].expect("vector slot span");
        assert!(span.offset as usize >= std::mem::size_of::<VectorObj>());
        assert_eq!(span.len, 2);
        let view = MappedHeapView::from_mut_slice(&mut heap.bytes);
        let header = view.veclike_header_mut(object_span).unwrap();
        assert_eq!(header.cast::<u8>(), heap.bytes.as_mut_ptr());
        assert_eq!(view.veclike_type(object_span).unwrap(), VecLikeType::Vector);
        let ptr = view
            .typed_object_mut::<VectorObj>(object_span, "vector")
            .unwrap();
        assert_eq!(ptr.cast::<u8>(), heap.bytes.as_mut_ptr());
    }

    #[test]
    fn reserves_mapped_cons_cells_as_heap_objects() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![
                DumpHeapObject::Cons {
                    car: crate::emacs_core::pdump::types::DumpValue::Int(1),
                    cdr: crate::emacs_core::pdump::types::DumpValue::Int(2),
                },
                DumpHeapObject::Cons {
                    car: crate::emacs_core::pdump::types::DumpValue::Int(3),
                    cdr: crate::emacs_core::pdump::types::DumpValue::Nil,
                },
            ],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let mut heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(heap.bytes.len(), 2 * std::mem::size_of::<ConsCell>());
        assert_eq!(tagged_heap.mapped_cons.len(), 2);
        let first = tagged_heap.mapped_cons[0].expect("first cons span");
        let second = tagged_heap.mapped_cons[1].expect("second cons span");
        assert_eq!(first.offset, 0);
        assert_eq!(second.offset as usize, std::mem::size_of::<ConsCell>());

        let view = MappedHeapView::from_mut_slice(&mut heap.bytes);
        let ptr = view.cons_cell_mut(first).unwrap();
        assert_eq!(ptr.cast::<u8>(), heap.bytes.as_mut_ptr());

        assert_eq!(
            read_usize(&heap.bytes, first.offset as usize),
            TaggedValue::fixnum(1).bits()
        );
        assert_eq!(
            read_usize(
                &heap.bytes,
                first.offset as usize + std::mem::size_of::<TaggedValue>()
            ),
            TaggedValue::fixnum(2).bits()
        );
    }

    #[test]
    fn reserves_mapped_float_objects_as_heap_objects() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Float(1.0), DumpHeapObject::Float(2.0)],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let mut heap = extract_tagged_heap_payloads(&mut tagged_heap);
        assert_eq!(heap.bytes.len(), 2 * std::mem::size_of::<FloatObj>());
        assert_eq!(tagged_heap.mapped_floats.len(), 2);
        let first = tagged_heap.mapped_floats[0].expect("first float span");
        let second = tagged_heap.mapped_floats[1].expect("second float span");
        assert_eq!(first.offset, 0);
        assert_eq!(second.offset as usize, std::mem::size_of::<FloatObj>());

        let view = MappedHeapView::from_mut_slice(&mut heap.bytes);
        let ptr = view.float_obj_mut(first).unwrap();
        assert_eq!(ptr.cast::<u8>(), heap.bytes.as_mut_ptr());

        assert_eq!(
            heap.bytes[first.offset as usize + 1],
            HeapObjectKind::Float as u8
        );
        let value_offset = first.offset as usize + std::mem::size_of::<RawGcHeader>();
        let value = f64::from_ne_bytes(
            heap.bytes[value_offset..value_offset + std::mem::size_of::<f64>()]
                .try_into()
                .unwrap(),
        );
        assert_eq!(value, 1.0);
    }

    #[test]
    fn emits_tagged_relocations_for_heap_values_in_raw_cons_payload() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![
                DumpHeapObject::Str {
                    data: DumpByteData::owned(b"child".to_vec()),
                    size: 5,
                    size_byte: 5,
                    text_props: Vec::new(),
                },
                DumpHeapObject::Cons {
                    car: crate::emacs_core::pdump::types::DumpValue::Str(
                        crate::emacs_core::pdump::types::DumpHeapRef { index: 0 },
                    ),
                    cdr: crate::emacs_core::pdump::types::DumpValue::Nil,
                },
            ],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        let cons_span = tagged_heap.mapped_cons[1].expect("mapped cons");
        let string_span = tagged_heap.mapped_strings[0].expect("mapped string");

        assert!(
            heap.relocations
                .iter()
                .any(|relocation| relocation.location_offset == cons_span.offset
                    && relocation.addend == TAG_STRING as u8)
        );
        assert_eq!(
            read_usize(&heap.bytes, cons_span.offset as usize),
            string_span.offset as usize
        );
    }

    #[test]
    fn writes_raw_vector_slots_into_mapped_heap_payload() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Vector(vec![
                crate::emacs_core::pdump::types::DumpValue::Int(11),
                crate::emacs_core::pdump::types::DumpValue::True,
            ])],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        let slots = tagged_heap.mapped_slots[0].expect("mapped slots");
        let second = slots.offset as usize + std::mem::size_of::<TaggedValue>();

        assert_eq!(
            read_usize(&heap.bytes, slots.offset as usize),
            TaggedValue::fixnum(11).bits()
        );
        assert_eq!(read_usize(&heap.bytes, second), TaggedValue::T.bits());
    }

    #[test]
    fn emits_value_fixups_for_raw_slots_that_need_runtime_remap() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![DumpHeapObject::Vector(vec![
                crate::emacs_core::pdump::types::DumpValue::Symbol(
                    crate::emacs_core::pdump::types::DumpSymId(42),
                ),
                crate::emacs_core::pdump::types::DumpValue::Subr(
                    crate::emacs_core::pdump::types::DumpNameId(7),
                ),
            ])],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);
        let slots = tagged_heap.mapped_slots[0].expect("mapped slots");

        assert_eq!(heap.value_fixups.len(), 2);
        assert!(matches!(
            heap.value_fixups[0],
            RawValueFixup::Symbol { location_offset } if location_offset == slots.offset
        ));
        assert!(matches!(
            heap.value_fixups[1],
            RawValueFixup::Value {
                location_offset,
                value: crate::emacs_core::pdump::types::DumpValue::Subr(_),
            } if location_offset == slots.offset + std::mem::size_of::<TaggedValue>() as u64
        ));
        assert_eq!(read_usize(&heap.bytes, slots.offset as usize), 42);
    }

    #[test]
    fn rebuild_heap_metadata_matches_extracted_layout() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![
                DumpHeapObject::Str {
                    data: DumpByteData::owned(b"abc".to_vec()),
                    size: 3,
                    size_byte: 3,
                    text_props: Vec::new(),
                },
                DumpHeapObject::Vector(vec![
                    crate::emacs_core::pdump::types::DumpValue::Int(1),
                    crate::emacs_core::pdump::types::DumpValue::Nil,
                ]),
                DumpHeapObject::Cons {
                    car: crate::emacs_core::pdump::types::DumpValue::Int(2),
                    cdr: crate::emacs_core::pdump::types::DumpValue::Nil,
                },
            ],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };
        let _heap = extract_tagged_heap_payloads(&mut tagged_heap);
        let expected_cons = tagged_heap.mapped_cons.clone();
        let expected_strings = tagged_heap.mapped_strings.clone();
        let expected_veclikes = tagged_heap.mapped_veclikes.clone();
        let expected_slots = tagged_heap.mapped_slots.clone();

        clear_heap_metadata(&mut tagged_heap);
        rebuild_heap_metadata(&mut tagged_heap).expect("rebuild heap metadata");

        assert_eq!(tagged_heap.mapped_cons, expected_cons);
        assert_eq!(tagged_heap.mapped_strings, expected_strings);
        assert_eq!(tagged_heap.mapped_veclikes, expected_veclikes);
        assert_eq!(tagged_heap.mapped_slots, expected_slots);
    }

    #[test]
    fn reserves_mapped_vectorlike_headers_as_heap_objects() {
        let mut tagged_heap = DumpTaggedHeap {
            objects: vec![
                DumpHeapObject::Vector(Vec::new()),
                DumpHeapObject::Record(Vec::new()),
                DumpHeapObject::Lambda(Vec::new()),
                DumpHeapObject::Macro(Vec::new()),
            ],
            mapped_cons: Vec::new(),
            mapped_floats: Vec::new(),
            mapped_strings: Vec::new(),
            mapped_veclikes: Vec::new(),
            mapped_slots: Vec::new(),
        };

        let heap = extract_tagged_heap_payloads(&mut tagged_heap);

        assert_eq!(tagged_heap.mapped_veclikes.len(), 4);
        assert_eq!(
            tagged_heap.mapped_veclikes[0].unwrap().len as usize,
            std::mem::size_of::<VectorObj>()
        );
        assert_eq!(
            tagged_heap.mapped_veclikes[1].unwrap().len as usize,
            std::mem::size_of::<RecordObj>()
        );
        assert_eq!(
            tagged_heap.mapped_veclikes[2].unwrap().len as usize,
            std::mem::size_of::<LambdaObj>()
        );
        assert_eq!(
            tagged_heap.mapped_veclikes[3].unwrap().len as usize,
            std::mem::size_of::<MacroObj>()
        );
        assert!(heap.bytes.len() >= std::mem::size_of::<VectorObj>());
    }

    fn read_usize(bytes: &[u8], offset: usize) -> usize {
        usize::from_ne_bytes(
            bytes[offset..offset + std::mem::size_of::<usize>()]
                .try_into()
                .unwrap(),
        )
    }

    fn read_i64(bytes: &[u8], offset: usize) -> i64 {
        i64::from_ne_bytes(
            bytes[offset..offset + std::mem::size_of::<i64>()]
                .try_into()
                .unwrap(),
        )
    }
}
