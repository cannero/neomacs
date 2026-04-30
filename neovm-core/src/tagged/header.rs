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
pub union ConsCdrOrNext {
    pub cdr: TaggedValue,
    pub next_free: *mut ConsCell,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct ConsCell {
    pub car: TaggedValue,
    pub cdr_or_next: ConsCdrOrNext,
}

impl ConsCell {
    #[inline]
    pub unsafe fn cdr(&self) -> TaggedValue {
        unsafe { self.cdr_or_next.cdr }
    }

    #[inline]
    pub unsafe fn set_car(&mut self, value: TaggedValue) {
        self.car = value;
    }

    #[inline]
    pub unsafe fn set_cdr(&mut self, value: TaggedValue) {
        self.cdr_or_next.cdr = value;
    }

    #[inline]
    pub unsafe fn free_next(&self) -> *mut ConsCell {
        unsafe { self.cdr_or_next.next_free }
    }

    #[inline]
    pub unsafe fn set_free_next(&mut self, next: *mut ConsCell) {
        self.car = TaggedValue::NIL;
        self.cdr_or_next.next_free = next;
    }
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
    /// Arbitrary-precision integer (like GNU's PVEC_BIGNUM).
    /// Mirrors `struct Lisp_Bignum` in `src/bignum.h`, which wraps an
    /// `mpz_t`. NeoMacs wraps `rug::Integer` (which itself wraps the
    /// same `mpz_t` from libgmp).
    Bignum = 13,
    /// Symbol with source position (like GNU's PVEC_SYMBOL_WITH_POS).
    /// Wraps a bare symbol + byte offset for byte-compiler diagnostics.
    SymbolWithPos = 14,
}

use std::sync::OnceLock;

/// Slot storage for vectorlike objects that can either be ordinary Rust-owned
/// storage or a borrowed slice in a mapped pdump image.
pub struct LispValueVec {
    storage: LispValueVecStorage,
}

#[repr(transparent)]
pub struct LispValueSlice([TaggedValue]);

impl LispValueSlice {
    pub fn from_slice(slice: &[TaggedValue]) -> &Self {
        unsafe { &*(slice as *const [TaggedValue] as *const Self) }
    }

    pub fn as_slice(&self) -> &[TaggedValue] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<TaggedValue> {
        self.0.to_vec()
    }

    pub fn clone(&self) -> Vec<TaggedValue> {
        self.to_vec()
    }
}

impl std::ops::Deref for LispValueSlice {
    type Target = [TaggedValue];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl std::fmt::Debug for LispValueSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl PartialEq<Vec<TaggedValue>> for LispValueSlice {
    fn eq(&self, other: &Vec<TaggedValue>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PartialEq<LispValueSlice> for Vec<TaggedValue> {
    fn eq(&self, other: &LispValueSlice) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<'a> IntoIterator for &'a LispValueSlice {
    type Item = &'a TaggedValue;
    type IntoIter = std::slice::Iter<'a, TaggedValue>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a &'a LispValueSlice {
    type Item = &'a TaggedValue;
    type IntoIter = std::slice::Iter<'a, TaggedValue>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

enum LispValueVecStorage {
    Owned(Vec<TaggedValue>),
    Mapped { ptr: *const TaggedValue, len: usize },
}

// Mapped slots are read-only through shared references.  Mutation paths use
// `ensure_owned` before exposing `&mut Vec<TaggedValue>`.
unsafe impl Send for LispValueVecStorage {}
unsafe impl Sync for LispValueVecStorage {}

impl LispValueVec {
    pub fn owned(items: Vec<TaggedValue>) -> Self {
        Self {
            storage: LispValueVecStorage::Owned(items),
        }
    }

    /// Build slot storage whose contents live in a mapped pdump image.
    ///
    /// # Safety
    /// `ptr..ptr+len` must remain mapped and immutable for the lifetime of the
    /// returned storage unless a mutation first copies the slots into owned
    /// storage.
    pub(crate) unsafe fn mapped(ptr: *const TaggedValue, len: usize) -> Self {
        Self {
            storage: LispValueVecStorage::Mapped { ptr, len },
        }
    }

    pub fn as_slice(&self) -> &[TaggedValue] {
        match self.storage {
            LispValueVecStorage::Owned(ref items) => items,
            LispValueVecStorage::Mapped { ptr, len } => {
                if len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(ptr, len) }
                }
            }
        }
    }

    pub fn ensure_owned(&mut self) -> &mut Vec<TaggedValue> {
        if let LispValueVecStorage::Mapped { .. } = self.storage {
            let items = self.as_slice().to_vec();
            self.storage = LispValueVecStorage::Owned(items);
        }
        match self.storage {
            LispValueVecStorage::Owned(ref mut items) => items,
            LispValueVecStorage::Mapped { .. } => {
                unreachable!("mapped vector storage was copied to owned slots")
            }
        }
    }

    pub fn owned_capacity(&self) -> usize {
        match self.storage {
            LispValueVecStorage::Owned(ref items) => items.capacity(),
            LispValueVecStorage::Mapped { .. } => 0,
        }
    }
}

impl From<Vec<TaggedValue>> for LispValueVec {
    fn from(value: Vec<TaggedValue>) -> Self {
        Self::owned(value)
    }
}

impl Clone for LispValueVec {
    fn clone(&self) -> Self {
        Self::owned(self.as_slice().to_vec())
    }
}

impl std::ops::Deref for LispValueVec {
    type Target = [TaggedValue];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl std::ops::DerefMut for LispValueVec {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ensure_owned().as_mut_slice()
    }
}

impl<'a> IntoIterator for &'a LispValueVec {
    type Item = &'a TaggedValue;
    type IntoIter = std::slice::Iter<'a, TaggedValue>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
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
    pub data: LispValueVec,
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
    pub data: LispValueVec,
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
    pub data: LispValueVec,
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
    pub data: LispValueVec,
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
/// Contains a GNU-shaped fixed-arity or variadic entry point together with
/// arity metadata stored on the SubrObj itself.
pub type SubrFnMany = fn(
    &mut crate::emacs_core::eval::Context,
    Vec<super::value::TaggedValue>,
) -> crate::emacs_core::error::EvalResult;
pub type SubrFnManySlice = fn(
    &mut crate::emacs_core::eval::Context,
    &[super::value::TaggedValue],
) -> crate::emacs_core::error::EvalResult;
pub type SubrFn0 =
    fn(&mut crate::emacs_core::eval::Context) -> crate::emacs_core::error::EvalResult;
pub type SubrFn1 = fn(
    &mut crate::emacs_core::eval::Context,
    super::value::TaggedValue,
) -> crate::emacs_core::error::EvalResult;
pub type SubrFn2 = fn(
    &mut crate::emacs_core::eval::Context,
    super::value::TaggedValue,
    super::value::TaggedValue,
) -> crate::emacs_core::error::EvalResult;
pub type SubrFn3 = fn(
    &mut crate::emacs_core::eval::Context,
    super::value::TaggedValue,
    super::value::TaggedValue,
    super::value::TaggedValue,
) -> crate::emacs_core::error::EvalResult;

#[derive(Clone, Copy)]
pub enum SubrFn {
    Many(SubrFnMany),
    ManySlice(SubrFnManySlice),
    A0(SubrFn0),
    A1(SubrFn1),
    A2(SubrFn2),
    A3(SubrFn3),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SubrDispatchKind {
    Builtin,
    ContextCallable,
    SpecialForm,
}

#[repr(C)]
pub struct SubrObj {
    pub header: VecLikeHeader,
    /// The canonical symbol identity for this primitive function.
    pub sym_id: crate::emacs_core::intern::SymId,
    /// The runtime-local name atom for the subr's public name.
    pub name: crate::emacs_core::intern::NameId,
    /// Minimum number of arguments.
    pub min_args: u16,
    /// Maximum number of arguments (None = unlimited/&rest).
    pub max_args: Option<u16>,
    /// How the evaluator should dispatch this public subr surface.
    pub dispatch_kind: SubrDispatchKind,
    /// Native Rust entry point for the builtin, if fully registered.
    pub function: Option<SubrFn>,
}

/// Heap-allocated arbitrary-precision integer (mirrors GNU
/// `struct Lisp_Bignum` in `src/bignum.h`).
///
/// GNU stores an `mpz_t` directly inside the struct. NeoMacs wraps
/// `rug::Integer`, which itself owns an `mpz_t` (from libgmp). The GC
/// has no Lisp_Object children to trace — the only owned resource is
/// the GMP-managed limb buffer, which is freed when `Drop` runs in
/// `free_gc_object`.
#[repr(C)]
pub struct BignumObj {
    pub header: VecLikeHeader,
    pub value: rug::Integer,
}

/// A symbol annotated with its source byte offset.
/// Mirrors GNU `struct Lisp_Symbol_With_Pos` (`lisp.h:958`).
/// Both fields are `TaggedValue` (GC-traced), matching GNU's LISPSIZE=2.
#[repr(C)]
pub struct SymbolWithPosObj {
    pub header: VecLikeHeader,
    /// The bare symbol. Must always be a plain symbol (TAG_SYMBOL).
    pub sym: TaggedValue,
    /// Source byte offset. Must always be a fixnum.
    pub pos: TaggedValue,
}

#[cfg(test)]
mod tests {
    use super::{LispValueSlice, LispValueVec};
    use crate::tagged::value::TaggedValue;

    #[test]
    fn mapped_lisp_value_vec_borrows_until_mutation() {
        let slots = vec![TaggedValue::fixnum(1), TaggedValue::fixnum(2)];
        let mut values = unsafe { LispValueVec::mapped(slots.as_ptr(), slots.len()) };

        assert_eq!(values.as_slice(), slots.as_slice());
        values.ensure_owned().push(TaggedValue::fixnum(3));

        drop(slots);
        assert_eq!(
            values.as_slice(),
            &[
                TaggedValue::fixnum(1),
                TaggedValue::fixnum(2),
                TaggedValue::fixnum(3)
            ]
        );
    }

    #[test]
    fn lisp_value_slice_clone_returns_owned_vec_for_compat_callers() {
        let slots = vec![TaggedValue::fixnum(1), TaggedValue::fixnum(2)];
        let slice = LispValueSlice::from_slice(&slots);

        let owned = slice.clone();
        drop(slots);
        assert_eq!(owned, vec![TaggedValue::fixnum(1), TaggedValue::fixnum(2)]);
    }
}
