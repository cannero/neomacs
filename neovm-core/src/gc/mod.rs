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
