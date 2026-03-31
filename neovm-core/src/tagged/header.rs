//! Heap object headers and layouts for the tagged pointer GC.
//!
//! # Object categories
//!
//! **Cons cells** — no header, just `(car, cdr)` = 16 bytes.
//! GC uses an external mark bitmap in the cons block allocator.
//!
//! **Strings, Floats** — have a `GcHeader` for mark bit and sweep list.
//!
//! **Vectorlike objects** — have a `VecLikeHeader` (extends `GcHeader`)
//! with a `type_tag` field distinguishing vectors, hash tables, lambdas,
//! macros, bytecode, buffers, markers, overlays, records, etc.

use super::value::TaggedValue;

// ---------------------------------------------------------------------------
// ConsCell — no header, minimal size
// ---------------------------------------------------------------------------

/// A cons cell: two tagged values, no header.
///
/// 16 bytes on 64-bit. GC marks cons cells via an external bitmap
/// in the block allocator, not via an in-object flag.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ConsCell {
    pub car: TaggedValue,
    pub cdr: TaggedValue,
}

// ---------------------------------------------------------------------------
// GcHeader — shared header for all non-cons heap objects
// ---------------------------------------------------------------------------

/// GC header prepended to every non-cons heap object.
///
/// Provides mark bit for garbage collection and an intrusive linked list
/// pointer for sweep-phase traversal.
#[repr(C)]
pub struct GcHeader {
    /// Mark bit: set during mark phase, cleared before each GC cycle.
    pub marked: bool,
    /// Intrusive linked list of all GC-managed objects (for sweep).
    pub next: *mut GcHeader,
}

impl GcHeader {
    pub fn new() -> Self {
        Self {
            marked: false,
            next: std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// Typed heap objects
// ---------------------------------------------------------------------------

/// Heap-allocated string object.
#[repr(C)]
pub struct StringObj {
    pub header: GcHeader,
    pub data: crate::gc::types::LispString,
}

/// Heap-allocated float object.
#[repr(C)]
pub struct FloatObj {
    pub header: GcHeader,
    pub value: f64,
}

// ---------------------------------------------------------------------------
// Vectorlike — catch-all for complex heap types
// ---------------------------------------------------------------------------

/// Sub-type tag for vectorlike objects.
/// Stored in the `VecLikeHeader`, distinguishes the many heap types
/// that share the `011` pointer tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VecLikeType {
    Vector = 0,
    HashTable = 1,
    Lambda = 2,
    Macro = 3,
    ByteCode = 4,
    Record = 5,
    Overlay = 6,
    Marker = 7,
    Buffer = 8,
    Window = 9,
    Frame = 10,
    Timer = 11,
}

/// Header for all vectorlike heap objects.
///
/// Extends `GcHeader` with a type tag. The type-specific data follows
/// this header in memory (accessed via pointer cast to the concrete type).
#[repr(C)]
pub struct VecLikeHeader {
    pub gc: GcHeader,
    pub type_tag: VecLikeType,
}

impl VecLikeHeader {
    pub fn new(type_tag: VecLikeType) -> Self {
        Self {
            gc: GcHeader::new(),
            type_tag,
        }
    }
}

// -- Concrete vectorlike types --

/// Heap-allocated vector (dynamic array of Values).
#[repr(C)]
pub struct VectorObj {
    pub header: VecLikeHeader,
    pub data: Vec<TaggedValue>,
}

/// Heap-allocated hash table.
#[repr(C)]
pub struct HashTableObj {
    pub header: VecLikeHeader,
    pub table: crate::emacs_core::value::LispHashTable,
}

/// Heap-allocated lambda (interpreted closure).
#[repr(C)]
pub struct LambdaObj {
    pub header: VecLikeHeader,
    pub data: crate::emacs_core::value::LambdaData,
}

/// Heap-allocated macro.
#[repr(C)]
pub struct MacroObj {
    pub header: VecLikeHeader,
    pub data: crate::emacs_core::value::LambdaData,
}

/// Heap-allocated bytecode function.
#[repr(C)]
pub struct ByteCodeObj {
    pub header: VecLikeHeader,
    pub data: crate::emacs_core::bytecode::ByteCodeFunction,
}

/// Heap-allocated record (like vector with a type tag in slot 0).
#[repr(C)]
pub struct RecordObj {
    pub header: VecLikeHeader,
    pub data: Vec<TaggedValue>,
}

/// Heap-allocated overlay.
#[repr(C)]
pub struct OverlayObj {
    pub header: VecLikeHeader,
    pub data: crate::gc::types::OverlayData,
}

/// Heap-allocated marker.
#[repr(C)]
pub struct MarkerObj {
    pub header: VecLikeHeader,
    pub data: crate::gc::types::MarkerData,
}

/// Heap-allocated buffer reference (wraps a BufferId).
#[repr(C)]
pub struct BufferObj {
    pub header: VecLikeHeader,
    pub id: crate::buffer::BufferId,
}

/// Heap-allocated window reference (wraps a u64 id).
#[repr(C)]
pub struct WindowObj {
    pub header: VecLikeHeader,
    pub id: u64,
}

/// Heap-allocated frame reference (wraps a u64 id).
#[repr(C)]
pub struct FrameObj {
    pub header: VecLikeHeader,
    pub id: u64,
}

/// Heap-allocated timer reference (wraps a u64 id).
#[repr(C)]
pub struct TimerObj {
    pub header: VecLikeHeader,
    pub id: u64,
}
