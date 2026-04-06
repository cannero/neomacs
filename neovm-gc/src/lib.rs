#![forbid(unsafe_op_in_unsafe_fn)]
//! `neovm-gc` is a standalone managed-heap crate for VM runtimes.
//!
//! The crate provides a managed object model, rooted handles, descriptor-driven
//! tracing, stop-the-world collection, and the heap/control surfaces needed by
//! a future concurrent generational collector.

pub mod background;
pub mod barrier;
mod collector_exec;
mod collector_state;
pub mod descriptor;
pub mod edge;
pub mod heap;
mod index_state;
mod mark;
pub mod mutator;
mod object;
pub mod plan;
pub mod root;
pub mod runtime;
mod runtime_state;
pub mod spaces;
pub mod stats;
pub mod weak;

pub use background::{
    BackgroundCollectionRuntime, BackgroundCollector, BackgroundCollectorConfig,
    BackgroundCollectorStats, BackgroundService, BackgroundWorker, BackgroundWorkerConfig,
    BackgroundWorkerError, BackgroundWorkerStats, BackgroundWorkerStatus, SharedBackgroundError,
    SharedBackgroundObservation, SharedBackgroundService, SharedBackgroundServiceStatus,
    SharedBackgroundStatus, SharedBackgroundWaitResult, SharedHeap, SharedHeapAccessError,
    SharedHeapError, SharedHeapStatus,
};
pub use barrier::{BarrierEvent, BarrierKind, RememberedEdge};
pub use descriptor::{
    EphemeronVisitor, GcErased, LayoutKind, MovePolicy, Relocator, Trace, TraceFn, Tracer,
    TypeDesc, TypeFlags, WeakProcessor, trace_edge,
};
pub use edge::EdgeCell;
pub use heap::{AllocError, Heap, HeapConfig};
pub use mutator::Mutator;
pub use object::estimated_allocation_size;
pub use plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
pub use root::{Gc, HandleScope, Root};
pub use runtime::{CollectorRuntime, SharedCollectorRuntime};
pub use stats::{CollectionStats, HeapStats, OldRegionStats, SpaceStats};
pub use weak::{Ephemeron, Weak, WeakCell, WeakMapToken};

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
