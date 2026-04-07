#![forbid(unsafe_op_in_unsafe_fn)]
// Several internal GC orchestration entry points (e.g.
// `prepare_full_reclaim_for_plan`, `record_active_major_*_and_refresh`,
// `completed_old_gen_cycle`) take many arguments by design — they
// thread the small-and-orthogonal pieces of `Heap` state through
// the collector pipeline without building an intermediate "context"
// struct that would just shuffle the same fields around. Suppressing
// `too_many_arguments` crate-wide acknowledges that choice rather
// than papering over each function with its own #[allow].
#![allow(clippy::too_many_arguments)]
// Every public item must have rustdoc. Promoted to deny so the
// crate stays fully documented at the public surface and future
// additions cannot accidentally land undocumented.
#![deny(missing_docs)]
//! `neovm-gc` is a standalone managed-heap crate for VM runtimes.
//!
//! The crate provides a managed object model, rooted handles, descriptor-driven
//! tracing, and a generational collector with the following pieces:
//!
//! * **Nursery**: bump-pointer semispace allocator with parallel evacuation
//!   into per-worker sub-arenas (see [`spaces`]).
//! * **Old generation**: Immix-style block pool with line marks, hole-filling
//!   allocation, and per-block card tables.
//! * **Physical old-gen compaction**: opt-in pass that evacuates surviving
//!   records out of sparse blocks into freshly-packed target blocks,
//!   reclaims the now-empty source blocks, and surfaces cumulative
//!   compaction telemetry. Configured via
//!   `OldGenConfig::physical_compaction_density_threshold` (0.0 = disabled
//!   default) or invoked manually via `Heap::compact_old_gen_physical`,
//!   `compact_old_gen_aggressive`, and `compact_old_gen_if_fragmented`.
//! * **Concurrent marker**: dedicated lock-alternating mark thread that drives
//!   active major-mark sessions to completion via brief read-lock slices
//!   (see [`concurrent_marker`]).
//! * **Adaptive pacer**: Go-style EWMA-driven trigger model with three stacked
//!   constraints (heap growth, max pause budget, CPU-aware budget) and
//!   optional nursery soft trigger for early minor cycles (see [`pacer`]).
//! * **Telemetry**: rolling pause-time histogram, per-space heap stats,
//!   physical compaction counters ([`CompactionStats`]), and lock-free
//!   shared snapshots ([`PauseHistogram`], [`HeapStats`],
//!   [`SharedHeap::status`]).
//!
//! Most VM runtimes will interact with the crate through [`Heap`] (single
//! mutator) or [`SharedHeap`] (multi-thread observation, background workers).
//!
//! [`SharedHeap::status`]: background::SharedHeap::status
//!
//! # Quick start
//!
//! Construct a [`Heap`], grab a [`Mutator`] view from it, and read or
//! mutate state through the mutator. Cumulative pacer and compaction
//! counters are available via the heap's stats accessors regardless of
//! whether anything has been allocated yet.
//!
//! ```
//! use neovm_gc::{Heap, HeapConfig};
//!
//! let mut heap = Heap::new(HeapConfig::default());
//!
//! // Pacer telemetry: a fresh heap has observed nothing yet.
//! let pacer_stats = heap.pacer_stats();
//! assert_eq!(pacer_stats.observed_cycles, 0);
//!
//! // Compaction telemetry: zero physical compaction work so far.
//! let compaction_stats = heap.compaction_stats();
//! assert_eq!(compaction_stats.cycles, 0);
//! assert_eq!(compaction_stats.records_moved, 0);
//!
//! // Old-gen fragmentation ratio reads 0.0 on an empty pool.
//! assert_eq!(heap.old_gen_fragmentation_ratio(), 0.0);
//!
//! // The should_compact predicate returns false at any threshold
//! // when no blocks exist.
//! assert!(!heap.should_compact_old_gen(0.5));
//! ```

/// Shared/background collector surfaces (`SharedHeap`,
/// background worker, lock-free status snapshots).
pub mod background;
/// Write-barrier event types and remembered-set edge metadata.
pub mod barrier;
pub(crate) mod card_table;
mod collector_exec;
mod collector_policy;
mod collector_session;
mod collector_state;
/// Phase 5 dedicated concurrent-marker scaffold built on top of
/// `BackgroundWorker`.
pub mod concurrent_marker;
/// Type descriptors and tracing/relocation traits the collector
/// dispatches through (`Trace`, `Tracer`, `Relocator`,
/// `TypeDesc`, `MovePolicy`).
pub mod descriptor;
/// Strong managed-edge helper used inside `Trace`-implementing
/// records (`EdgeCell`).
pub mod edge;
/// The owned `Heap` type plus its configuration and allocation
/// errors.
pub mod heap;
mod index_state;
mod mark;
/// `Mutator<'heap>` — the only allocating view onto a `Heap`.
pub mod mutator;
mod object;
/// Adaptive Go-style pacer with EWMA trigger thresholds.
pub mod pacer;
mod pause_stats;
/// Collection plans the collector consumes
/// (`CollectionKind`, `CollectionPlan`, `CollectionPhase`,
/// `RuntimeWorkStatus`, etc.).
pub mod plan;
mod reclaim;
/// Rooted handles (`Gc`, `Root`, `HandleScope`, `RootStack`).
pub mod root;
/// Runtime collector entry points used inside the heap mutator
/// closures (`CollectorRuntime`, `SharedCollectorRuntime`).
pub mod runtime;
mod runtime_state;
/// Per-space configuration and metadata
/// (`NurseryConfig`, `OldGenConfig`, `PinnedSpaceConfig`,
/// `LargeObjectSpaceConfig`).
pub mod spaces;
/// Per-space statistics and rolling collection counters.
pub mod stats;
/// Weak reference, weak map, and ephemeron primitives.
pub mod weak;

pub use background::{
    BackgroundCollectionRuntime, BackgroundCollector, BackgroundCollectorConfig,
    BackgroundCollectorStats, BackgroundService, BackgroundWorker, BackgroundWorkerConfig,
    BackgroundWorkerError, BackgroundWorkerStats, BackgroundWorkerStatus, SharedBackgroundError,
    SharedBackgroundObservation, SharedBackgroundService, SharedBackgroundServiceStatus,
    SharedBackgroundStatus, SharedBackgroundWaitResult, SharedHeap, SharedHeapAccessError,
    SharedHeapError, SharedHeapStatus,
};
pub use concurrent_marker::{
    ConcurrentMarker, ConcurrentMarkerConfig, ConcurrentMarkerError, ConcurrentMarkerStats,
    ConcurrentMarkerStatus,
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
pub use pacer::{Pacer, PacerAllocationSpace, PacerConfig, PacerDecision, PacerStats};
pub use pause_stats::PauseHistogram;
pub use plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
pub use root::{Gc, HandleScope, Root};
pub use runtime::{CollectorRuntime, SharedCollectorRuntime};
pub use stats::{CollectionStats, CompactionStats, HeapStats, OldRegionStats, SpaceStats};
pub use weak::{Ephemeron, Weak, WeakCell, WeakMapToken};

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
