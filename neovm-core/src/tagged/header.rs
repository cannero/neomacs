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
use crate::buffer::text_props::TextPropertyTable;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HeapObjectKind {
    String = 0,
    Float = 1,
    VecLike = 2,
}

#[repr(C)]
pub struct GcHeader {
    /// Mark bit: set during mark phase, cleared before each GC cycle.
    pub marked: bool,
    /// Exact object category for typed sweep/deallocation.
    pub kind: HeapObjectKind,
    /// Intrusive linked list of all GC-managed objects (for sweep).
    pub next: *mut GcHeader,
}

impl GcHeader {
    pub fn new(kind: HeapObjectKind) -> Self {
        Self {
            marked: false,
            kind,
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
    pub data: crate::heap_types::LispString,
    /// GNU-compatible ownership: string text properties live on the string.
    pub text_props: TextPropertyTable,
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
    /// Built-in function (like GNU's PVEC_SUBR).
    Subr = 12,
}

use std::sync::OnceLock;

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
            gc: GcHeader::new(HeapObjectKind::VecLike),
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
///
/// Matches GNU Emacs's PVEC_CLOSURE: a plain vector of Lisp_Object slots.
/// The GC traces ALL slots uniformly — no type-specific tracing needed.
///
/// Slot layout (GNU Emacs compatible):
///   [0] CLOSURE_ARGLIST    — parameter list (e.g., (x y &optional z))
///   [1] CLOSURE_CODE       — body forms as Lisp list (interpreted) or bytecode
///   [2] CLOSURE_CONSTANTS  — lexical environment (interpreted) or constants vector
///   [3] CLOSURE_STACK_DEPTH — nil for interpreted, fixnum for bytecode
///   [4] CLOSURE_DOC_STRING — docstring or doc-form
///   [5] CLOSURE_INTERACTIVE — interactive spec
///   [6..] extra slots for oclosures
#[repr(C)]
pub struct LambdaObj {
    pub header: VecLikeHeader,
    /// All closure data as GC-managed Value slots.
    pub data: Vec<super::value::TaggedValue>,
    /// Parsed lambda params cached from slot 0 for fast calls/arity checks.
    pub parsed_params: OnceLock<crate::emacs_core::value::LambdaParams>,
}

/// Closure slot indices matching GNU Emacs (lisp.h).
pub const CLOSURE_ARGLIST: usize = 0;
pub const CLOSURE_CODE: usize = 1;
pub const CLOSURE_CONSTANTS: usize = 2;
pub const CLOSURE_STACK_DEPTH: usize = 3;
pub const CLOSURE_DOC_STRING: usize = 4;
pub const CLOSURE_INTERACTIVE: usize = 5;
/// Minimum number of slots in a closure vector.
pub const CLOSURE_MIN_SLOTS: usize = 6;

/// Heap-allocated macro — same layout as Lambda but with VecLikeType::Macro.
#[repr(C)]
pub struct MacroObj {
    pub header: VecLikeHeader,
    pub data: Vec<super::value::TaggedValue>,
    /// Parsed lambda params cached from slot 0 for fast calls/arity checks.
    pub parsed_params: OnceLock<crate::emacs_core::value::LambdaParams>,
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
    pub data: crate::heap_types::OverlayData,
}

/// Heap-allocated marker.
#[repr(C)]
pub struct MarkerObj {
    pub header: VecLikeHeader,
    pub data: crate::heap_types::MarkerData,
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

/// Heap-allocated built-in function (like GNU's PVEC_SUBR).
/// Contains the function pointer, arity, and name symbol.
pub type SubrFn = fn(
    &mut crate::emacs_core::eval::Context,
    Vec<super::value::TaggedValue>,
) -> crate::emacs_core::error::EvalResult;

#[repr(C)]
pub struct SubrObj {
    pub header: VecLikeHeader,
    /// The SymId of the subr's name (e.g., intern("car")).
    pub name: crate::emacs_core::intern::SymId,
    /// Minimum number of arguments.
    pub min_args: u16,
    /// Maximum number of arguments (None = unlimited/&rest).
    pub max_args: Option<u16>,
    /// Native Rust entry point for the builtin, if fully registered.
    pub function: Option<SubrFn>,
}
