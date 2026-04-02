//! Garbage Collector for the NeoVM Elisp runtime.
//!
//! The primary GC is now the tagged pointer system in `crate::tagged::gc`.
//! The old LispHeap/ObjId system in `heap.rs`/`types.rs` is retained only for
//! legacy unit tests and is no longer part of the runtime surface.

#[cfg(test)]
pub(crate) mod heap;
#[cfg(test)]
pub(crate) mod objects;
#[cfg(test)]
pub(crate) mod types;

#[cfg(test)]
pub(crate) use heap::LispHeap;
#[cfg(test)]
pub(crate) use objects::*;
#[cfg(test)]
pub(crate) use types::{HeapObject, ObjId};

use crate::emacs_core::value::Value;

/// Trait for types that hold GC-managed `Value` references.
///
/// Each sub-manager implements this to enumerate all `Value`s it holds,
/// so the mark-and-sweep collector can discover every live object.
pub trait GcTrace {
    /// Push all `Value` references held by `self` into `roots`.
    fn trace_roots(&self, roots: &mut Vec<Value>);
}
