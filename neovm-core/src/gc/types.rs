//! GC heap object types and handles.

use crate::emacs_core::bytecode::ByteCodeFunction;
use crate::emacs_core::value::{LambdaData, LispHashTable, Value};
pub use crate::heap_types::{LispString, MarkerData, OverlayData};

/// Handle to a heap-allocated object.  Copy-able, 8 bytes.
///
/// `index` selects the slot in `LispHeap::objects`.
/// `generation` detects use-after-free (stale handles panic on access).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjId {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

impl std::fmt::Debug for ObjId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjId({}/{})", self.index, self.generation)
    }
}

pub enum HeapObject {
    Cons {
        car: Value,
        cdr: Value,
    },
    Vector(Vec<Value>),
    HashTable(LispHashTable),
    Str(LispString),
    Lambda(LambdaData),
    Macro(LambdaData),
    ByteCode(ByteCodeFunction),
    Overlay(OverlayData),
    Marker(MarkerData),
    /// Freed slot, available for reuse.
    Free,
}

impl HeapObject {
    /// Collect all `Value` references contained in this object (for GC marking).
    pub fn trace_values(&self) -> Vec<Value> {
        match self {
            HeapObject::Cons { car, cdr } => vec![*car, *cdr],
            HeapObject::Vector(v) => v.clone(),
            HeapObject::HashTable(ht) => ht
                .data
                .values()
                .copied()
                .chain(ht.key_snapshots.values().copied())
                .collect(),
            HeapObject::Str(_) => Vec::new(),
            HeapObject::Lambda(d) | HeapObject::Macro(d) => {
                let mut vals: Vec<Value> = d.env.into_iter().collect();
                if let Some(doc_val) = d.doc_form {
                    vals.push(doc_val);
                }
                if let Some(interactive_val) = d.interactive {
                    vals.push(interactive_val);
                }
                vals
            }
            HeapObject::ByteCode(bc) => {
                let mut vals: Vec<Value> = bc.constants.clone();
                if let Some(env_val) = bc.env {
                    vals.push(env_val);
                }
                if let Some(doc_val) = bc.doc_form {
                    vals.push(doc_val);
                }
                if let Some(interactive_val) = bc.interactive {
                    vals.push(interactive_val);
                }
                vals
            }
            HeapObject::Overlay(overlay) => vec![overlay.plist],
            HeapObject::Marker(_) => Vec::new(),
            HeapObject::Free => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
