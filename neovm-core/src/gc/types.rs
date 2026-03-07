//! GC heap object types and handles.

use crate::emacs_core::bytecode::ByteCodeFunction;
use crate::emacs_core::value::{LambdaData, LispHashTable, Value};

/// Handle to a heap-allocated object.  Copy-able, 8 bytes.
///
/// `index` selects the slot in `LispHeap::objects`.
/// `generation` detects use-after-free (stale handles panic on access).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjId {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

impl std::fmt::Debug for ObjId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjId({}/{})", self.index, self.generation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LispString {
    pub text: String,
    pub multibyte: bool,
}

/// The concrete object stored on the managed heap.
///
/// All heap-allocated Lisp types live here: cons cells, vectors, hash tables,
/// strings, lambdas, macros, and bytecode functions.
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
            HeapObject::Lambda(d) | HeapObject::Macro(d) => d.env.into_iter().collect(),
            HeapObject::ByteCode(bc) => {
                let mut vals: Vec<Value> = bc.constants.clone();
                if let Some(env_val) = bc.env {
                    vals.push(env_val);
                }
                vals
            }
            HeapObject::Free => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
