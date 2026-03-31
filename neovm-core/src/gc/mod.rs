//! Garbage Collector for the NeoVM Elisp runtime.
//!
//! The primary GC is now the tagged pointer system in `crate::tagged::gc`.
//! The old LispHeap/ObjId system in `heap.rs`/`types.rs` is retained for
//! pdump compatibility but is no longer used at runtime.

pub mod heap;
pub mod objects;
pub mod types;

pub use heap::LispHeap;
pub use objects::*;
pub use types::{HeapObject, ObjId};

use crate::emacs_core::value::Value;

/// Trait for types that hold GC-managed `Value` references.
///
/// Each sub-manager implements this to enumerate all `Value`s it holds,
/// so the mark-and-sweep collector can discover every live object.
pub trait GcTrace {
    /// Push all `Value` references held by `self` into `roots`.
    fn trace_roots(&self, roots: &mut Vec<Value>);
}
