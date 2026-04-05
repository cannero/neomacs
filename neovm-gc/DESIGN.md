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
- thread-local allocation buffers
- copied into survivor space or promoted into old generation

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
- thread-local
- lock-free for the common case

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
    api.rs
    barrier.rs
    descriptor.rs
    heap.rs
    mutator.rs
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
```

Suggested ownership:

- `api.rs`: public entry points
- `root.rs`: `Root`, `HandleScope`, root stack
- `descriptor.rs`: `Trace`, `Tracer`, `TypeDesc`
- `spaces/*`: space-specific allocation/reclamation
- `barrier.rs`: write barriers and remembered-set plumbing
- `plan.rs`: collection cycle state machine
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
