//! Garbage Collector for the NeoVM Elisp runtime.
//!
//! # Architecture
//!
//! Arena-based mark-and-sweep collector:
//!
//! - **LispHeap**: Arena that owns all cycle-forming objects (cons, vector, hash-table).
//! - **ObjId**: Lightweight 8-byte handle (index + generation) replacing `Arc<Mutex<T>>`.
//! - **Thread-local access**: The evaluator sets a thread-local pointer before evaluation;
//!   `Value` constructors and accessors use it transparently.
//! - **Mark-and-sweep**: Iterative worklist marking from root set, sweep frees unmarked objects.
//! - **Generation counters**: Catch use-after-collected bugs at runtime (stale ObjId panics).

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
