//! Heap-allocated object types for the pointer-based Value system.
//!
//! Each heap object (except ConsCell) has a `GcHeader` for mark-sweep GC.
//! ConsCell has no header — GC uses an external mark bitmap in the block allocator.

use crate::emacs_core::value::{LambdaData, LispHashTable, Value};
use crate::emacs_core::bytecode::ByteCodeFunction;

// ---------------------------------------------------------------------------
// GC Header
// ---------------------------------------------------------------------------

/// Mark bit + intrusive sweep list pointer for heap objects.
#[repr(C)]
pub struct GcHeader {
    /// Set during mark phase, cleared at start of each GC cycle.
    pub marked: bool,
    /// Intrusive linked list for sweep traversal.
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
// ConsCell — headerless, 16 bytes
// ---------------------------------------------------------------------------

/// A cons cell: just car + cdr, no header.
/// GC marks via external bitmap in the cons block allocator.
#[repr(C)]
pub struct ConsCell {
    pub car: Value,
    pub cdr: Value,
}

// ---------------------------------------------------------------------------
// Typed heap objects (with GcHeader)
// ---------------------------------------------------------------------------

/// Heap-allocated string.
#[repr(C)]
pub struct StringObj {
    pub header: GcHeader,
    pub data: crate::gc::types::LispString,
}

/// Heap-allocated float.
#[repr(C)]
pub struct FloatObj {
    pub header: GcHeader,
    pub value: f64,
}

/// Heap-allocated vector.
#[repr(C)]
pub struct VectorObj {
    pub header: GcHeader,
    pub data: Vec<Value>,
}

/// Heap-allocated hash table.
#[repr(C)]
pub struct HashTableObj {
    pub header: GcHeader,
    pub table: LispHashTable,
}

/// Heap-allocated lambda (interpreted closure).
#[repr(C)]
pub struct LambdaObj {
    pub header: GcHeader,
    pub data: LambdaData,
}

/// Heap-allocated macro.
#[repr(C)]
pub struct MacroObj {
    pub header: GcHeader,
    pub data: LambdaData,
}

/// Heap-allocated bytecode function.
#[repr(C)]
pub struct ByteCodeObj {
    pub header: GcHeader,
    pub data: ByteCodeFunction,
}

/// Heap-allocated record.
#[repr(C)]
pub struct RecordObj {
    pub header: GcHeader,
    pub data: Vec<Value>,
}

/// Heap-allocated overlay.
#[repr(C)]
pub struct OverlayObj {
    pub header: GcHeader,
    pub data: super::types::OverlayData,
}

/// Heap-allocated marker.
#[repr(C)]
pub struct MarkerObj {
    pub header: GcHeader,
    pub data: super::types::MarkerData,
}
