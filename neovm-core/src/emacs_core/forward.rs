//! Forwarder descriptors for `SYMBOL_FORWARDED` symbols.
//!
//! Mirrors GNU Emacs's `Lisp_Fwd` family in `src/lisp.h:3060-3145`. A
//! forwarded symbol stores a pointer to a static [`LispFwd`] descriptor;
//! reads and writes go through the descriptor instead of touching the
//! symbol's value cell directly. This is how variables like
//! `buffer-file-name`, `point`, `mark-active`, and `case-fold-search`
//! get their backing storage in dedicated C-side slots.
//!
//! # Phase 1
//!
//! This file is a **skeleton**. The redirect dispatch in
//! [`crate::emacs_core::symbol::SymbolRedirect::Forwarded`] does not yet
//! reach this code; Phase 8 of the symbol-redirect refactor
//! (`drafts/symbol-redirect-plan.md`) is where the forwarder family
//! goes live. The types are declared here so the union variant on
//! [`crate::emacs_core::symbol::SymbolVal::fwd`] can name them.

use super::intern::SymId;
use super::value::Value;

/// Discriminant for [`LispFwd`]. Mirrors GNU `enum Lisp_Fwd_Type`
/// (`src/lisp.h:3046-3055`). Always read the first field of any `*Fwd`
/// struct to determine its concrete type — exactly the GNU trick.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LispFwdType {
    /// `Lisp_Intfwd`: forward to a static `intmax_t`.
    Int = 0,
    /// `Lisp_Boolfwd`: forward to a static `bool`.
    Bool = 1,
    /// `Lisp_Objfwd`: forward to a static `Lisp_Object` (a top-level
    /// global variable).
    Obj = 2,
    /// `Lisp_Buffer_Objfwd`: forward to a slot inside the current
    /// buffer's per-buffer storage.
    BufferObj = 3,
    /// `Lisp_Kboard_Objfwd`: forward to a slot inside the current
    /// keyboard's per-kboard storage.
    KboardObj = 4,
}

/// Common header. Every `Lisp_*Fwd` struct begins with this so the
/// dispatch code can read the discriminant from a `*const LispFwd`
/// without knowing the concrete type. Mirrors GNU `lispfwd` (`lisp.h:760`)
/// + the `type` field on each `Lisp_*fwd` body (`lisp.h:3060-3094`).
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct LispFwd {
    pub ty: LispFwdType,
    // The body fields differ per variant. Code that has a `*const LispFwd`
    // matches on `ty` and re-casts to the concrete `Lisp*Fwd` pointer.
}

/// `Lisp_Intfwd`: forward to a static integer. Phase 8 wires this up.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LispIntFwd {
    pub ty: LispFwdType,
    pub get: fn() -> i64,
    pub set: fn(i64),
}

/// `Lisp_Boolfwd`: forward to a static bool. Phase 8 wires this up.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LispBoolFwd {
    pub ty: LispFwdType,
    pub get: fn() -> bool,
    pub set: fn(bool),
}

/// `Lisp_Objfwd`: forward to a static `Value` global. Phase 8 wires this up.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LispObjFwd {
    pub ty: LispFwdType,
    pub get: fn() -> Value,
    pub set: fn(Value),
}

/// `Lisp_Buffer_Objfwd`: forward to a per-buffer slot. The `offset`
/// field indexes into `Buffer::slots: [Value; BUFFER_SLOT_COUNT]`,
/// playing the same role as GNU's `Lisp_Buffer_Objfwd::offset` (a byte
/// offset into `struct buffer`). Phase 8 wires this up.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LispBufferObjFwd {
    pub ty: LispFwdType,
    /// Index into `Buffer::slots`. Mirrors GNU `Lisp_Buffer_Objfwd::offset`.
    pub offset: u16,
    /// Index into `buffer_local_flags` for "is this buffer-local in the
    /// current buffer?" tests. -1 means "always local everywhere",
    /// matching GNU's `PER_BUFFER_IDX(idx) == -1`.
    pub local_flags_idx: i16,
    /// Optional Lisp predicate symbol checked on every write. Mirrors
    /// GNU `store_symval_forwarding`'s predicate path.
    pub predicate: SymId,
    /// Default value copied into `Buffer::slots[offset]` at buffer
    /// creation. Mirrors GNU `buffer_defaults`.
    pub default: Value,
}

/// `Lisp_Kboard_Objfwd`: forward to a per-keyboard slot. Phase 8 stubs
/// this with a single global `KBoard`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LispKboardObjFwd {
    pub ty: LispFwdType,
    pub offset: u16,
}
