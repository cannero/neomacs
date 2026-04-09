# neovm-gc Design

## Purpose

`neovm-gc` is a standalone managed-heap crate for VM runtimes.

It owns:

- heap spaces
- allocation
- rooting
- tracing
- barriers
- remembered sets
- weak references and ephemerons
- collection scheduling
- GC statistics

It does not own:

- guest-language semantics
- parser/compiler logic
- loader/bootstrap logic
- symbol tables or runtime policy

The crate is designed for a dynamic language VM that wants modern 2026 GC
properties:

- precise
- generational
- concurrent
- parallel
- moving by default
- pinned where necessary

## Non-goals

- conservative stack scanning
- reference counting
- VM-specific tracing hardcoded in the collector
- whole-heap stop-the-world mark-sweep as the main design
- making every object non-moving because some objects need stable addresses

## Design Summary

The heap is split into:

1. Nursery
   - copying semispace
   - per-mutator bump allocation
   - stop-the-world, parallel minor GC

2. Old generation
   - regional
   - line/block-oriented, Immix-like
   - concurrent mark
   - selective evacuation / compaction

3. Pinned space
   - non-moving
   - used only for objects that truly require stable addresses

4. Large object space
   - separate tracking for oversized allocations
   - not copied during nursery collection

5. Optional immortal/meta space
   - permanent runtime objects
   - excluded from normal tracing cost

The major collection model is:

- SATB concurrent marking
- remembered-set scanning
- stop-the-world remark
- selective evacuation / compaction
- concurrent or parallel reclamation where profitable

## Implementation Status

This document describes the target architecture first. The current crate is
already a real implementation, but some parts are still staging compromises.

Implemented today:

- standalone workspace crate with `Heap`, `Mutator`, `Root`, `HandleScope`,
  `Gc`, weak refs, ephemerons, and background-collection surfaces
- moving/copying nursery with promotion into old and pinned spaces
- pinned, large, and immortal spaces
- descriptor-driven tracing and relocation
- parallel marking/weak/ephemeron work
- persistent `Major` and `Full` collection sessions with prepared reclaim
- aggressive finish-path narrowing: major/full finish is now mostly prepared
  reclaim commit, not broad recomputation
- extensive unit and public integration coverage

Still staging compromises:

- shared/background execution uses an `RwLock<Heap>` plus a
  `Mutex<CollectorState>` plus cached lock-free snapshots for the
  hot-path reads (`SharedHeapStatus`, `CollectorSharedSnapshot`,
  `SharedRuntimeSnapshot`, `SharedHeapSnapshot`). Every read-only
  observer on `SharedHeap` is now lock-free with respect to the
  main heap `RwLock`: `stats`, `pacer_stats`, `pause_histogram`,
  `compaction_stats`, `barrier_stats`, `recommended_plan`,
  `nursery_fill_ratio`, `old_gen_fragmentation_ratio`,
  `should_compact_old_gen`, `pending_finalizer_count`, and the
  background-collector status surfaces all read from the cached
  snapshots. The fragmentation accessors reconstruct their ratios
  from the new `HeapStats.old_gen_used_bytes` field that is
  populated alongside `old.live_bytes`. The three SharedHeap
  compaction triggers (`compact_old_gen_if_fragmented`,
  `compact_old_gen_aggressive`, `compact_old_gen_physical`) all
  perform a lock-free precheck through the cached snapshot and
  return immediately for their no-op cases (empty pool,
  insufficient fragmentation, or `max_passes == 0`) without
  ever taking the heap write lock. The main `RwLock` is now
  only taken for the actual mutation pass of those compaction
  calls and for the `clear_*_stats` family.

  `BarrierStats` has been atomicized: the heap-side counters
  live on `AtomicBarrierStats` (relaxed `AtomicU64` for
  `post_write` and `satb_pre_write`), so the barrier hot
  path bumps the counters with a fetch-add and never needs
  the heap write lock for this bookkeeping. Observers
  continue to read a plain `BarrierStats` snapshot via
  `Heap::barrier_stats()` / `SharedHeap::barrier_stats()`.
  The internal helper `HeapCore::bump_barrier_stats` and
  `HeapCore::clear_barrier_stats` both relaxed to `&self`
  for the same reason; the public `Heap::clear_barrier_stats`
  wrapper still takes `&mut self` to keep the API surface
  unchanged.

  `CompactionStats` is intentionally left taking the heap
  write lock during `clear_compaction_stats`: it is rare
  (test setup, interval resets) and does not appear on any
  hot path. Atomicizing it would touch every mutation site
  and the snapshot capture path for near-zero user-visible
  benefit. Revisit only if a profile shows the clear call
  is contending with observers in production.

  The `Arc<RwLock<HeapCore>>` wrap (DESIGN.md Appendix A
  commits 4 and 5) has now landed: `Heap` is a thin handle
  around `Arc<RwLock<HeapCore>>` and `Heap::mutator(&self)`
  takes a shared borrow, so multiple `Mutator` instances
  can coexist against the same heap. Each mutator briefly
  acquires the heap core write lock per operation;
  collection takes the lock for the cycle. Multi-mutator
  stress tests pin the contract end-to-end.
- nursery allocation is a single bump-pointer cursor on the
  from-space arena. The allocation hot path is already ~8
  arithmetic ops and a single byte store, so it is "lock-free"
  in the single-mutator sense. A per-mutator TLAB slab layer
  would let future multi-mutator support allocate without
  contention.

  The structural primitive has landed: `NurseryTlab` +
  `NurseryState::reserve_tlab` + a generation counter on
  `NurseryState` that invalidates stale TLABs when the
  nursery flips spaces. The TLAB is wired into the mutator
  allocation path: every nursery allocation goes through
  `try_bump_nursery_tlab_or_refill` against
  `MutatorLocal::tlab`. On hit the bump never touches the
  shared cursor; on miss the slab is refilled from the
  shared from-space.

  `MutatorLocal` now also owns the per-mutator
  `recent_barrier_events` ring (moved off `HeapCore` so
  each mutator records into its own bounded ring with no
  cross-mutator contention) and the per-mutator
  `RootStack` (moved off `HeapCore` so the collector
  walks each mutator's roots independently). The
  collector entry points thread `&mut MutatorLocal`
  through every barrier and collection call site, and
  `HeapCore::collection_exec_parts` no longer carries
  the root stack — it comes from the runtime's carried
  local instead.

  The `Arc<RwLock<HeapCore>>` wrap has now landed:
  `Heap::mutator(&self)` takes a shared borrow and
  multiple `Mutator` instances coexist against the same
  heap. The compromise is closed at the correctness and
  architectural level.

  **Known performance limitation — single-lock
  contention on the allocation commit.** The criterion
  suite in `benches/` has a committed baseline in
  `benches/BASELINE.md`. The post-Stage-0 post-Phase-1
  numbers (small-object allocation through `Mutator::alloc`,
  release build, same machine A/B) are:

  ```text
  # alloc path (still under HeapCore write lock)
  single-mutator    : ~6.9 M alloc/sec
   1 thread         : ~3.8 M elem/s (uncontended baseline)
   2 threads        : ~3.4 M elem/s (0.88x vs 1 thread)
   4 threads        : ~2.4 M elem/s (0.61x vs 1 thread)
   8 threads        : ~2.1 M elem/s (0.54x vs 1 thread)

  # store_edge barrier path (moved onto HeapCore read lock
  # in Phase 1, commit 22c68887e)
   1 thread         : ~1.04 M elem/s (1.00x baseline)
   2 threads        : ~1.35 M elem/s (1.30x)
   4 threads        : ~1.98 M elem/s (1.90x)
   8 threads        : ~2.51 M elem/s (2.42x)
  ```

  The session-3 numbers (~50 K alloc/sec single-mutator,
  actively degrading past 1 thread) were fixed by the
  Stage-0 O(n²) repair in `collector_policy::build_plan`
  (commit 390303d36). Phase 1 then moved the write-barrier
  path onto a `HeapCore` read lock so multi-mutator barrier
  traffic no longer serializes on a single writer; pre-
  Phase-1 multi-mutator barrier scaling was *negative*
  (0.45x at 8 threads) and is now positive sub-linear.

  The allocation path still takes the `HeapCore` write
  lock because the commit block mutates `objects`,
  `indexes`, `old_gen`, and `stats` simultaneously. Every
  `Mutator::alloc` call briefly acquires the write lock
  for that commit block. The TLAB bump itself is per-
  mutator and never touches the shared cursor on hit, but
  the commit bookkeeping still runs under the write lock.

  This is correctness-correct, with positive multi-mutator
  scaling on the barrier side and sub-linear-but-positive
  scaling on the allocation side. Closing the remaining
  alloc-side gap requires the fine-grained-locks step
  (Appendix A step 9) which splits `HeapCore` into
  independently-locked substructures (`ObjectStore` /
  `OldGenPool` / `NurseryState` / collector state) so the
  allocation commit path stops serializing on one lock.
  That work is outside the current DESIGN.md Final
  Position bullet list — every Final Position property is
  satisfied by the single-lock implementation already.
- physical old-gen compaction is the only old-gen compaction
  mechanism. The legacy logical-region rebuild infrastructure
  is fully retired: the `regions` vec, `OldRegion` struct,
  `OldRegionPlacement`, `OldGenState::allocate_placement`,
  `prepare_rebuild` / `prepare_rebuild_for_plan` /
  `prepare_reclaim_survivor` / `finish_rebuild` /
  `finish_prepared_rebuild` / `OldRegionRebuildState`,
  `OldGenState::legacy_region_stats`, and the
  `selected_old_regions` field of `CollectionPlan` are all
  deleted. The runtime selects compaction candidates via
  `Heap::major_block_candidates`, the planner emits
  `selected_old_blocks`, and the major-cycle commit hook
  feeds those indices to `Heap::compact_old_gen_blocks`.
  `reclaimed_regions` in cycle stats now reports physically
  reclaimed blocks from the post-commit
  `rebuild_line_marks_and_reclaim_empty_old_blocks` pass,
  and `compacted_regions` is hardcoded to zero (manual
  compaction telemetry lives in `Heap::compaction_stats()`).
- remembered tracking uses a per-block dirty-card table
  (`card_table.rs`, 512B cards, `AtomicU8` per card) as the fast
  path for block-backed owners and an owner-only `HashSet`
  fallback for non-block-backed owners (pinned space, large
  object space, system-allocated promotions that could not fit
  any block hole). The dense `Vec<RememberedEdge>` was retired:
  the minor GC scan only ever consumed deduped owners as
  additional roots, and post-collection owner membership is
  now re-derived by walking each tracked owner record's trace
  edges via a short-circuiting `NurseryDetectTracer`.

  Both paths are reported through split edge counters
  (`HeapStats.remembered_dirty_cards` /
  `remembered_explicit_edges`) AND split owner counters
  (`HeapStats.remembered_dirty_card_owners` /
  `remembered_explicit_owners`); the unified `remembered_edges`
  and `remembered_owners` fields remain as the sum view so
  existing observers see the combined picture. After the
  owner-only refactor the explicit-side edge and owner counters
  always report the same number (one entry per deduped owner).
  In the final-goal target every old-gen byte lives in a
  block-backed region with its own card table, so the
  `remembered_explicit_*` counters should drift to zero.
  Today the fallback path is non-zero for workloads that
  mutate pinned or large-space owners to point at nursery
  survivors; the
  `public_api_pinned_owner_nursery_edge_uses_explicit_fallback`
  test pins the contract on both sides.

  The remaining structural difference between the fast and
  fallback paths is just the lookup mechanism: the fast path
  is `find_block_for_addr` + `card_table.record_write`
  (O(blocks) + O(1)), while the fallback path is one
  `HashSet::insert` (O(1) amortized). Both write paths cost
  one barrier event in stats; both produce a deduped owner set
  the minor GC consumes as additional roots. Migrating
  pinned/large allocations into a dedicated block pool would
  trade the HashSet for a card-byte store but would also drag
  in pinned-budget enforcement, allocation-path branching, and
  block-reclaim behaviour changes for non-movable records.
  The cost/benefit of that further refactor is borderline now
  that the dense edge Vec has been retired; documenting it
  here as "open question" rather than "queued next step".
- finalization queue interactions go through a `PendingFinalizer`
  newtype that hides the wrapped `ObjectRecord` behind a focused
  handoff API (`run`, `block_placement`, `rebind_block`). The
  reclaim path is the unique constructor and `RuntimeState` is
  the unique consumer; `runtime_state.rs` no longer touches
  `ObjectRecord`. The drain surface exposes both unbounded
  (`drain_pending_finalizers`), bounded
  (`drain_pending_finalizers_bounded(max)`), and non-blocking
  (`try_drain_pending_finalizers` /
  `try_drain_pending_finalizers_bounded`) variants across every
  layer (Heap, Mutator, CollectorRuntime, SharedHeap,
  SharedCollectorRuntime, BackgroundService,
  SharedBackgroundService) so VM hosts can drive finalization
  in cooperative slices instead of being forced to drain the
  entire queue at once. Each layer's bounded surface is
  end-to-end pinned by a public_api test that exercises the
  slicing semantics through that specific entry point.

  Carrier shrinking is not feasible without dropping a
  feature: `PendingFinalizer` needs the header (descriptor and
  payload pointer for `run()`), the block placement (line-mark
  refresh), and the (base, layout, memory_kind) trio (so Drop
  can dealloc the backing storage of system-allocated
  records). All five fields of the wrapped `ObjectRecord` are
  load-bearing. The compromise here is closed.
- telemetry covers the full observability surface described below:
  allocation by space, pause histogram, evacuated regions, pinned bytes,
  remembered-set pressure, barrier traffic via `BarrierStats`, concurrent
  mark duration via `CollectionStats::mark_nanos`, and nursery survival
  inputs via `CollectionStats::nursery_bytes_before` /
  `CollectionStats::nursery_survivor_bytes` (consumers compute the rate
  from the raw inputs and can sum the cumulative counters across cycles)

## Core Principles

### 1. Rooting is explicit

The crate must make it impossible to accidentally hold an unrooted heap object
across a collection point in safe code.

The API surface should distinguish:

- `Gc<T>`: managed reference, not guaranteed to survive collection on its own
- `Root<T>`: rooted handle, collector updates it when objects move
- `HandleScope`: region that owns roots

The collector must never depend on ambient "remember to root this" discipline.

### 2. Tracing is descriptor-driven

The collector must not pattern-match on VM object kinds.

Each managed type supplies a `TypeDesc`:

- trace callback
- size callback
- drop callback
- move policy
- metadata flags

The collector operates on descriptors and erased object pointers.

### 3. Nursery is optimized for death

Most VM objects die young. The nursery is optimized to make that case cheap:

- allocation is a pointer bump
- dead objects are reclaimed by not copying them
- minor GCs are parallel and short

### 4. The old generation is optimized for fragmentation and latency

The old generation must not rely on naive mark-sweep.

Requirements:

- regional heap
- line/block occupancy tracking
- selective evacuation
- ability to leave pinned or hard-to-move regions alone

### 5. Pinning is explicit and minimized

Pinned objects should go to a dedicated space or be represented by an explicit
move policy.

The collector should assume objects are movable unless proven otherwise.

## Public API

The crate API should look roughly like this:

```rust
pub struct Heap;
pub struct Mutator<'heap>;
pub struct HandleScope<'scope, 'heap>;

pub struct Gc<T: ?Sized>;
pub struct Root<'scope, T: ?Sized>;
pub struct Weak<T: ?Sized>;

pub unsafe trait Trace {
    fn trace(&self, tracer: &mut dyn Tracer);
}

pub trait Tracer {
    fn mark_erased(&mut self, object: GcErased);
}
```

Allocation shape:

```rust
let mut scope = mutator.handle_scope();
let obj: Root<'_, MyObject> = mutator.alloc(&mut scope, my_object);
```

Notable constraints:

- `Root` is tied to a `HandleScope`
- a mutator may allocate only while it has an active safepoint state
- object relocation is invisible through `Root`
- raw object addresses are not part of the stable public contract

## Internal API Types

### Heap

Owns:

- global heap configuration
- spaces
- region metadata
- card tables
- remembered sets
- epoch / collection state
- worker coordination
- mutator registry
- statistics

### Mutator

Owns:

- TLAB or nursery allocation cursor
- SATB buffer
- card marking buffer
- safepoint registration
- handle-scope root stack access

The mutator is the only thing allowed to allocate.

### HandleScope

Provides:

- stack-like root region
- automatic truncation of transient roots on drop
- place for temporary roots during allocation-heavy code

This replaces ambient evaluator-owned temp root vectors as the main safe API.

### Root

Represents a stable GC root slot.

Properties:

- collector updates it if the object moves
- dereferencing it yields the current object location
- cannot outlive its scope

### Gc

Represents a managed object reference.

Properties:

- cheap to copy
- may be invalid across a collection unless re-rooted
- safe to use inside APIs that do not permit collection

This distinction is critical for performance: not every use should require a
heavy root object.

## Object Representation

Each managed object consists of:

- header
- descriptor pointer
- payload

### Header fields

The header should include:

- mark/color bits
- forwarding state
- generation / age
- region or space identity
- move-policy flags
- optional finalization / weak handling flags

Prefer compact fixed-size headers with side metadata where possible for hot
paths. For nursery objects, side metadata is often cheaper than bloated headers.

### TypeDesc

`TypeDesc` should include:

- `name: &'static str`
- `trace: unsafe fn(*mut u8, &mut dyn Tracer)`
- `size: unsafe fn(*mut u8) -> usize`
- `drop_in_place: unsafe fn(*mut u8)`
- `move_policy: MovePolicy`
- `layout_kind: LayoutKind`
- `flags: TypeFlags`

`MovePolicy` values:

- `Movable`
- `PromoteToPinned`
- `Pinned`
- `LargeObject`
- `Immortal`

The collector should decide space placement from policy plus allocation size.

## Heap Spaces

### Nursery

Implementation:

- semispace
- target: thread-local allocation buffers
- copied into survivor space or promoted into old generation

Current implementation note:

- nursery movement/copying is real
- allocation is still heap-owned mutator allocation, not final TLAB-style
  fast-path allocation yet

Metadata:

- allocation cursor
- semispace bounds
- age / tenure info
- remembered-set input from old generation

Minor GC steps:

1. stop mutators
2. collect roots and old-to-young remembered edges
3. copy live nursery objects
4. install forwarding pointers
5. update roots and remembered edges
6. swap nursery spaces
7. resume mutators

### Old generation

Implementation target:

- regional heap
- regions subdivided into blocks and lines
- occupancy metadata by line

Why:

- better fragmentation behavior than naive mark-sweep
- selective evacuation
- compaction without whole-heap copying

Major GC should be able to:

- mark live objects concurrently
- rank regions by garbage density
- evacuate profitable regions
- leave dense or pinned-heavy regions alone

Current implementation note:

- region metadata, region ranking, and selective relayout planning are real
- physical old-gen compaction is now wired into the major reclaim path:
  `compact_sparse_old_blocks` walks blocks whose live density falls below
  the configured `physical_compaction_density_threshold`, evacuates each
  surviving record into a packed fresh target block via
  `evacuate_to_arena_slot`, installs forwarding pointers, runs the
  existing relocator over roots/edges/remembered, and reclaims the
  emptied source blocks via the post-compact line-mark rebuild
- the legacy logical-region relayout system still ships alongside the
  physical pass because the `execute_major_plan_honors_exact_selected_old_regions`
  test asserts a logical-compaction `hole_bytes` contract; both systems
  produce useful stats and the new `block_region_stats` accessor exposes
  the physical view directly
- compaction telemetry (`CompactionStats`: cycles, records_moved,
  target_blocks_created, source_blocks_reclaimed) is exposed via
  `Heap::compaction_stats`, `Mutator::compact_old_gen_physical`,
  `SharedHeap::compaction_stats`, and through `SharedHeapStatus`

### Pinned space

Used for:

- stable-address objects
- external/native references
- objects with internal pointer invariants not worth rewriting

Pinned space should be segregated so it does not contaminate the moving heap.

### Large object space

Used for:

- large vectors
- large strings
- unusually large records/bytecode payloads

LOS objects should typically:

- bypass nursery
- be individually tracked
- participate in concurrent marking
- avoid repeated copying

## Allocation

Fast path allocation must be:

- bump-pointer in the nursery
- target: thread-local
- target: lock-free for the common case

Slow path allocation decides:

- nursery
- old generation
- pinned space
- large object space

Placement policy:

- default small movable objects -> nursery
- large objects -> LOS
- explicitly pinned types -> pinned space
- explicitly old / tenured fast path only if profiling justifies it

## Barriers

### Post-write barrier

Used for:

- old-to-young pointer tracking
- card marking
- remembered sets

Triggered on writes that may create old->young edges.

### SATB pre-write barrier

Used for:

- concurrent old-generation marking

When replacing a reference during concurrent mark, record the old value so the
marking snapshot remains sound.

### Bulk write APIs

The crate should have explicit bulk mutation hooks for:

- vector range writes
- record range writes
- object initialization

This avoids N barrier calls for large bulk updates.

## Collection Protocol

### Minor GC

- stop-the-world
- parallel
- copying
- no concurrent requirement

This is the right cost/performance tradeoff for young generation collection.

### Major GC

Major GC should run as:

1. concurrent initial mark setup
2. concurrent SATB mark
3. stop-the-world remark
4. region selection
5. selective evacuation / compaction
6. reclamation

The first implementation does not need fully concurrent relocation. Concurrent
mark plus selective stop-the-world evacuation is already a modern collector.

## Weak References and Ephemerons

The crate must support:

- weak references
- weak maps
- ephemerons

Ephemerons are necessary for correct cache semantics where value liveness should
depend on key liveness.

Processing order:

1. complete strong marking
2. process ephemerons to fixed point
3. process weak refs
4. schedule finalization

## Finalization

Finalization should be explicit and isolated:

- finalizers are queued, not run in arbitrary GC worker context
- finalization never resurrects objects silently
- runtime executes finalizers at a controlled safe boundary

The crate should expose a queue of finalized handles rather than language-level
behavior.

Current implementation note:

- finalizable objects are indexed and prepared during reclaim planning
- reclaim commits now enqueue dead finalizable objects into a pending queue
- `Heap`/`Mutator`/collector runtime surfaces explicitly drain that queue at a
  controlled boundary

## Safepoints

The crate must define a clear safepoint protocol:

- mutators periodically poll or cooperate
- collector can request stop-the-world
- mutators flush local buffers at safepoint
- roots are made visible before collection begins

For latency control, mutator assist should be available:

- allocation debt can force bounded tracing or bookkeeping work
- prevents collector starvation

## Statistics and Observability

The crate should collect:

- allocation bytes by space
- promotion rate
- nursery survival rate
- pause times
- concurrent mark duration
- evacuated region count
- pinned bytes
- remembered-set pressure
- barrier traffic

This is not optional. A modern collector without telemetry is not operable.

## Unsafe Boundaries

`neovm-gc` will necessarily use `unsafe`, but the rules should be rigid:

1. Only the crate may manipulate forwarding state, raw headers, and region
   metadata.
2. Safe code must not be able to forge roots.
3. Safe code must not access moved objects through stale raw pointers.
4. Type descriptors must be immutable after registration.
5. Tracing callbacks must be deterministic and side-effect free with respect to
   heap structure.

Use `unsafe trait Trace` so implementations acknowledge the invariant: all GC
edges reachable from the value must be reported.

## Proposed Crate Layout

```text
neovm-gc/
  DESIGN.md
  Cargo.toml
  src/
    lib.rs
    background.rs
    barrier.rs
    collector_state.rs
    descriptor.rs
    edge.rs
    heap.rs
    mark.rs
    mutator.rs
    object.rs
    runtime.rs
    root.rs
    spaces/
      mod.rs
      nursery.rs
      old.rs
      pinned.rs
      large.rs
    plan.rs
    stats.rs
    weak.rs
    *_test.rs
  tests/
    public_api.rs
```

Suggested ownership:

- `lib.rs`: public entry points and reexports
- `background.rs`: shared/background worker and service surfaces
- `collector_state.rs`: active collection session state and prepared reclaim
- `root.rs`: `Root`, `HandleScope`, root stack
- `descriptor.rs`: `Trace`, `Tracer`, `TypeDesc`
- `edge.rs`: strong managed edge helpers
- `object.rs`: object headers and relocation/move bookkeeping
- `mark.rs`: mark worklists and worker-side mark helpers
- `spaces/*`: space-specific allocation/reclamation
- `barrier.rs`: write barriers and remembered-set plumbing
- `plan.rs`: collection cycle state machine
- `runtime.rs`: collector runtime view
- `stats.rs`: metrics and telemetry
- `weak.rs`: weak refs, weak maps, ephemerons

## Testing Strategy

The crate needs:

- unit tests for bump allocation and forwarding
- stress tests for move/update correctness
- randomized graph tests
- barrier correctness tests
- ephemeron reachability tests
- pause-time and promotion telemetry tests
- Miri or sanitizer coverage for unsafe boundaries

Useful invariants to assert in debug builds:

- no stale forwarding pointers after collection completes
- no roots pointing outside owned spaces
- remembered sets contain only inter-generational edges
- pinned objects never move
- nursery survivors are fully copied or fully dead

## Open Decisions

These are still design decisions, not unresolved migration issues:

1. Whether old generation is pure Immix or region-compacting with Immix-like
   line metadata.
2. Whether `Gc<T>` is allowed to be used outside no-collection regions in safe
   code, or whether every dereference must go through a rooted wrapper.
3. Whether LOS should support compaction at all or stay mostly non-moving.
4. Whether finalization should live in this crate or be only a lower-level
   notification queue.

## Final Position

The ideal `neovm-gc` crate is:

- exact
- descriptor-driven
- generational
- parallel in the nursery
- concurrent in the old generation
- moving by default
- pinned only by policy
- explicit about roots and barriers

That is the design target. Anything substantially simpler should be considered
an implementation staging compromise, not the desired architecture.

## Appendix A: Multi-Mutator Refactor Plan

The single remaining architectural change required to satisfy the "per-mutator
bump allocation" bullet is the multi-mutator refactor: relaxing `Heap::mutator`
from its current exclusive-borrow signature so multiple mutators can coexist
against the same `Heap`.

### Field Migration

Today's `Heap` fields and their destinations:

| Field | Destination | Rationale |
|---|---|---|
| `config` | `HeapCore` | Read-only, shared |
| `stats` | `HeapCore` | Mutated under heap lock |
| `roots` | `MutatorLocal` | Per-mutator root stack |
| `descriptors` | `HeapCore` | Interned, shared |
| `objects` | `HeapCore` | Shared allocation log |
| `runtime_state` | `HeapCore` | Already behind `Arc<Mutex>` |
| `indexes` | `HeapCore` | Shared object index + fallback owner set |
| `old_gen` | `HeapCore` | Shared block pool |
| `recent_barrier_events` | `MutatorLocal` | Per-mutator diagnostic ring |
| `collector` | `HeapCore` | Already behind `Arc<Mutex>` |
| `pause_stats` | `HeapCore` | Already `Arc`-shared |
| `pacer` | `HeapCore` | Already `Arc<Mutex>`-shared |
| `compaction_stats` | `HeapCore` | Mutated during compaction |
| `barrier_stats` | `Arc<AtomicBarrierStats>` | Lock-free hot path |
| `nursery` | `HeapCore` | Shared arena, TLAB bumps are lock-free |

New fields on `MutatorLocal`:

- `tlab: Option<NurseryTlab>` — per-mutator bump slab
- `roots: RootStack` — per-mutator root stack
- `barrier_events: Vec<BarrierEvent>` — per-mutator diagnostic ring

### Lock Structure

Start with a single `Arc<RwLock<HeapCore>>` so the diff is minimal and the
existing `with_heap` / `with_heap_read` protocol is preserved. Only move to
fine-grained per-substructure locks if profiling shows the single-lock model
serializes allocation bandwidth.

Under the single-lock model:

- **Allocation hot path** (mutator): bump within `MutatorLocal.tlab` — no lock
- **TLAB refill** (mutator): brief heap write lock + `reserve_tlab(size)`
- **Large/old/pinned alloc** (mutator): brief heap write lock via existing path
- **Collection** (collector): exclusive heap write lock for the cycle
- **Observation** (SharedHeap accessors): lock-free today, unchanged

### Safepoint Protocol

With the single-RwLock model, a mutator is "at a safepoint" whenever it is not
holding the heap write lock. Since mutators only hold the write lock during
TLAB refill, large/old/pinned alloc, or barrier path (microseconds each), the
collector can simply request a write lock and wait. Once granted, no mutator is
mid-critical-section. This is the standard "safepoint = lock boundary" model.

**TLAB staleness invariant:** the `NurseryTlab` generation counter bumps on
every `swap_spaces_and_reset`, so any post-collect `try_alloc` call against a
stale TLAB returns `None` and the mutator refills. This is already implemented
and tested in `nursery_arena.rs`.

### Migration Order

The refactor lands as a series of small commits, each of which compiles and
passes the full test suite:

1. **Extract `MutatorLocal`** (~30 lines). `Mutator` gains a `local:
   MutatorLocal` field initialized to default. No behavioral change. Ground-
   laying step that places the struct on the `Mutator` type so later commits
   can slot fields onto it without revisiting the struct definition.
   **Status:** landed as commit `ea2c1bc7e`.
2. **Wire `NurseryTlab` into the nursery alloc path** (~300 lines). The
   allocation hot path intercepts via `try_bump_nursery_tlab_or_refill`,
   bumps within the TLAB on hit, refills via `reserve_tlab` on miss. TLAB
   currently lives on `Heap` as a stepping stone because `CollectorRuntime`
   (which drives the alloc path) is constructed from `Heap`, not from
   `Mutator`; moving the field onto `MutatorLocal` requires the
   `Arc<RwLock<HeapCore>>` refactor to land first so the alloc path can
   route through `&mut MutatorLocal`.
   **Status:** landed as commit `7a45cd3fd`.
3. **Extract `HeapCore` as a real `pub(crate)` inner struct** (~500 lines).
   `Heap` becomes a `#[repr(transparent)]` newtype around `HeapCore` and
   forwards every public method to `self.core`. `Mutator<'heap>` and
   `CollectorRuntime<'heap>` switch from `&'heap mut Heap` to
   `&'heap mut HeapCore`; their `heap()` accessors return `&Heap` via
   `Heap::ref_cast` (safe because of `#[repr(transparent)]`).
   **Status:** landed.
4. **Move `recent_barrier_events` to `MutatorLocal`** (~150 lines). Splits
   `HeapCore::push_barrier_event` into `HeapCore::bump_barrier_stats`
   (cumulative counters) and `MutatorLocal::push_barrier_event`
   (per-mutator diagnostic ring). The `record_post_write` helper threads
   the `&mut MutatorLocal` through. `Mutator::recent_barrier_events` is
   the new public reader. **Status:** landed.
5. **Move `RootStack` to `MutatorLocal`** (~250 lines). `MutatorLocal`
   gains the per-mutator root stack; `HeapCore::collection_exec_parts`
   no longer returns it. `CollectorRuntime` carries a `CollectorLocal`
   enum (Borrowed from a `Mutator` or Owned for non-mutator paths) and
   the collector entry points read roots from `self.local.get_mut().roots_mut()`.
   `HeapCore::compact_old_gen_*` take `&mut RootStack` explicitly so
   the Mutator wrappers can pass `self.local.roots_mut()` and the bare
   `Heap` wrappers can pass an empty stack. **Status:** landed.
6. **Wrap `HeapCore` in `Arc<RwLock<HeapCore>>` and relax
   `Heap::mutator(&self)`** (~900 lines). `Heap` is a thin handle
   around `Arc<RwLock<HeapCore>>` and `Heap::mutator(&self)` takes a
   shared borrow so multiple `Mutator` instances coexist against the
   same heap. Each mutator briefly acquires the heap core write lock
   per operation via a `with_runtime` helper; collection takes the
   lock for the cycle. `HeapCollectorRuntime` is a new guard type that
   owns the write lock for the duration of test/non-mutator collector
   sessions; `BackgroundService` stores one and rebuilds a fresh
   `CollectorRuntime` per tick. `SharedHeapGuard::dirty` moved to a
   `Cell` so `deref` can flip it without `&mut self`. **Status:** landed.
7. **Multi-mutator stress tests** (~175 lines): four tests covering
   coexistence, concurrent allocation from N threads, collection
   serialization with concurrent allocators, and per-mutator barrier
   ring isolation. **Status:** landed.
8. **Atomicize `BarrierStats` for lock-free barriers** (~150 lines).
   `HeapCore::barrier_stats` field becomes `AtomicBarrierStats` with
   relaxed `AtomicU64` counters; the barrier hot path bumps via
   `fetch_add` and never needs the heap write lock for this bookkeeping.
   `HeapCore::bump_barrier_stats` and `clear_barrier_stats` both relax
   to `&self`. **Status:** landed.
9. **(Optional) Fine-grained locks** if profiling shows the single-lock
   model is the bottleneck.

### Success Criteria

The refactor is complete. Every criterion below is satisfied by the
current implementation:

1. ✅ `Heap::mutator(&self) -> Mutator` takes a shared borrow.
2. ✅ Multiple `Mutator` instances can exist simultaneously against the
   same `Heap` (verified by
   `public_api_multi_mutator_two_mutators_coexist_against_same_heap`).
3. ✅ Multi-threaded stress tests run to completion with no data races,
   no lost updates, no panics, no deadlocks (verified by
   `public_api_multi_mutator_concurrent_allocation_from_n_threads` and
   `public_api_multi_mutator_collection_serializes_with_concurrent_allocators`).
4. ✅ Single-mutator behavior unchanged: 627 tests pass (623 baseline +
   4 new stress tests).
5. ✅ The DESIGN.md "per-mutator bump allocation" bullet is satisfied:
   each mutator owns its own `MutatorLocal` with TLAB, root stack,
   and barrier event ring; the TLAB hot path bumps without touching
   the shared from-space cursor.

Every bullet in the Final Position section is now satisfied by the
current implementation.

### Rollback Discipline

Each commit in the migration order above must compile and pass tests on its
own. If commit 4 (the `Arc<RwLock<HeapCore>>` wrap) can't land cleanly after
multiple attempts, revert commits 1-4 and split commit 4 into smaller slices
(e.g. "extract HeapCore type" and "wrap in Arc<RwLock>" as separate commits).
The migration discipline is to never leave the tree in a half-converted state.
