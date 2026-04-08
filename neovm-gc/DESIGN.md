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
  calls and for the `clear_*_stats` family. The final
  data-plane split would target reducing write-side contention
  next, e.g. by routing barrier-event recording and remembered-
  set maintenance through their own locks.
- nursery allocation is a single bump-pointer cursor on the
  from-space arena. The allocation hot path is already ~8
  arithmetic ops and a single byte store, so it is "lock-free"
  in the single-mutator sense. A per-mutator TLAB slab layer
  would let future multi-mutator support allocate without
  contention; the structural split (per-slab cursor + slab
  reservation from the unified cursor) is not yet in place
  because `Heap::mutator` returns a `Mutator<'_>` with an
  exclusive `&mut Heap` borrow, which precludes concurrent
  mutators at the type level.
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
  path for block-backed owners and a `Vec<RememberedEdge>`
  fallback for non-block-backed owners (pinned space, large
  object space, system-allocated promotions that could not fit
  any block hole). Both paths are reported through split
  edge counters (`HeapStats.remembered_dirty_cards` /
  `remembered_explicit_edges`) AND split owner counters
  (`HeapStats.remembered_dirty_card_owners` /
  `remembered_explicit_owners`); the unified `remembered_edges`
  and `remembered_owners` fields remain as the sum view so
  existing observers see the combined picture. In the final-goal
  target every old-gen byte lives in a block-backed region with
  its own card table, so `remembered_explicit_edges` and
  `remembered_explicit_owners` should drift toward zero as
  pinned and large spaces migrate to the block model. Today the
  fallback path is non-zero for workloads that mutate pinned or
  large-space owners to point at nursery survivors; the
  `public_api_pinned_owner_nursery_edge_uses_explicit_fallback`
  test pins the contract on both sides.
- finalization queue interactions go through a `PendingFinalizer`
  newtype that hides the wrapped `ObjectRecord` behind a focused
  handoff API (`run`, `block_placement`, `rebind_block`). The
  reclaim path is the unique constructor and `RuntimeState` is
  the unique consumer; `runtime_state.rs` no longer touches
  `ObjectRecord`. The drain surface now exposes both an
  unbounded `drain_pending_finalizers()` and a bounded
  `drain_pending_finalizers_bounded(max)` variant across every
  layer (Heap, Mutator, CollectorRuntime, SharedHeap,
  SharedCollectorRuntime, BackgroundService,
  SharedBackgroundService) so VM hosts can drive finalization
  in cooperative slices instead of being forced to drain the
  entire queue at once. Further work: the wrapped record still
  owns the same fields as before (header, base, layout, block
  placement, memory kind), so the carrier size hasn't shrunk —
  the next iteration could split the carrier into a smaller
  payload+descriptor pair for embedders that want to drive
  finalization themselves on a separate thread without holding
  any heap-side lock.
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
