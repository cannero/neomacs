# rkyv Pdump Hard-Cutover for NeoMacs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current bincode/serde pdump with a single new format built around mmap-backed cons cells plus rkyv-serialized non-cons state. Target TTY startup: ~20ms release.

**Non-goals:** backward compatibility with old dump files, dual v1/v2 loaders, or temporary migration shims. Old dumps can be discarded and regenerated.

**Architecture:** hard cutover to one format. Cons cells are stored in a flat arena and become live after in-place patching. All non-cons heap objects are serialized in structured rkyv sections and rehydrated into the live tagged heap. The new loader keeps the public entry points `dump_to_file`, `load_from_dump`, `snapshot_evaluator`, and `restore_snapshot`, but replaces their implementations completely. Restore order is: load interner first, rehydrate heap, reset runtime caches, restore registries, call `Context::from_dump`, then run post-assembly fixups.

**Tech Stack:** `rkyv 0.8`, `memmap2`, `bytemuck`

---

## Current Runtime Constraints

The current pdump path lives in `pdump/mod.rs`, `pdump/types.rs`, and `pdump/convert.rs`.

- `load_from_dump(path)` deserializes `DumpContextState` with bincode, then calls `reconstruct_evaluator(state)` in `pdump/mod.rs`.
- `reconstruct_evaluator` loads the interner, preloads heap objects, resets thread-local caches, restores charset/fontset registries, calls `Context::from_dump(...)`, reinstalls `BUFFER_OBJFWD` forwarders, then finishes preload bookkeeping.
- `Context::from_dump(...)` in `eval.rs` is still the final evaluator assembly point and takes 22 individual arguments.
- The current dump value model is index-based: `DumpValue` refers to heap objects through `DumpHeapRef(u32)` and `convert.rs` resolves those indices through `TaggedLoadState`.

The new format does not reuse that representation. It keeps the current semantic coverage, but not the current wire format or loader internals.

---

## Hard-Cutover Rules

1. There is only one on-disk format after this refactor.
2. `dump_to_file` writes only the new format.
3. `load_from_dump` loads only the new format.
4. No version dispatch to the old bincode path.
5. Old dump files are rejected and regenerated.
6. The in-memory snapshot API stays supported, but it uses the same new image model as file-backed pdump.

That means the implementation should delete or replace the old bincode-specific dump types and conversion pipeline instead of carrying both systems in parallel.

---

## Design Overview

### Two Heap Strategies

**Cons cells:** stored in one contiguous arena as 16-byte cells. They have no `GcHeader`, match the live `ConsCell` size/layout, and are kept alive for the process lifetime by the dump backing store.

**Everything else:** serialized in structured form and rehydrated into normal heap allocations. This includes:

- strings
- floats
- vectors
- records
- hash tables
- lambdas
- macros
- bytecode objects
- overlays
- markers
- bignums
- subr handles
- buffer handles
- window handles
- frame handles
- timer handles

The handle-like wrapper objects above are heap-backed today, so they still need representation in the new format even though their semantic payload is mostly an ID.

### One Semantic Value Type, One Packed Slot Format

The old plan kept drifting because it tried to make one type serve two incompatible purposes:

- structured metadata/non-cons references
- raw 8-byte cons cell slots

The new design splits those concerns cleanly:

- `DumpValueRef`: structured archived value representation used in rkyv metadata and rkyv non-cons objects
- `DumpSlotWord(u64)`: packed 8-byte slot representation used only inside the cons arena

Both represent the same semantic value space, but only `DumpSlotWord` needs a fixed-width binary encoding.

### One Loader for File and In-Memory Snapshots

The new file loader and the in-memory snapshot restore path should share one core implementation:

- file-backed load uses `MmapMut`
- in-memory restore uses owned bytes

Both feed the same loader over section byte slices, and both end by attaching the cons backing store to `TaggedHeap`.

To support that, `DumpConsRegion` should own a backing enum rather than only an mmap:

```rust
enum DumpConsBacking {
    Mmap(memmap2::MmapMut),
    Bytes(Box<[u8]>),
}
```

That avoids keeping two different restore implementations just because tests use `restore_snapshot` and startup uses `load_from_dump`.

---

## In-Memory Image

The public snapshot API should no longer expose a v1-style `DumpContextState`. Replace it with a single image type that mirrors the new file sections:

```rust
pub struct RuntimeImageV2 {
    pub cons_arena: Vec<u8>,
    pub cons_count: u64,
    pub non_cons_section: Vec<u8>,
    pub metadata_section: Vec<u8>,
}
```

Public API after the refactor:

```rust
pub fn snapshot_evaluator(eval: &Context) -> Result<RuntimeImageV2, DumpError>;
pub fn snapshot_active_evaluator(eval: &mut Context) -> Result<RuntimeImageV2, DumpError>;
pub fn restore_snapshot(image: &RuntimeImageV2) -> Result<Context, DumpError>;
pub fn clone_evaluator(eval: &Context) -> Result<Context, DumpError>;
pub fn clone_active_evaluator(eval: &mut Context) -> Result<Context, DumpError>;
```

`clone_*` should simply snapshot to `RuntimeImageV2` and restore from it.

---

## File Format

```rust
#[repr(C)]
pub struct DumpHeader {
    pub magic: [u8; 8],          // b"NEODUMP2"
    pub version: u32,            // single live format version
    pub flags: u32,              // reserved for future use, currently 0
    pub cons_arena_offset: u64,
    pub cons_arena_len: u64,
    pub cons_count: u64,
    pub non_cons_offset: u64,
    pub non_cons_len: u64,
    pub metadata_offset: u64,
    pub metadata_len: u64,
    pub checksum: [u8; 32],      // SHA-256 of everything after the header
    pub reserved: [u8; 24],      // explicit pad to 128 bytes
}
// total: 128 bytes
```

File order:

1. header
2. cons arena
3. non-cons section
4. metadata section

There is no v1 payload, no legacy compatibility data, and no separate object table. The cons arena does not need one because every cell is fixed-size and every cell has exactly two slots.

---

## Structured Dump Values

`DumpValueRef` is the semantic value type used in archived metadata and archived non-cons objects.

```rust
#[derive(Archive, Serialize, Deserialize, Clone, Debug)]
pub enum DumpValueRef {
    Nil,
    True,
    Unbound,
    Fixnum(i64),
    Symbol(u32),       // SymId

    Cons(u32),         // cons arena cell index

    String(u32),
    Float(u32),
    Vector(u32),
    Record(u32),
    HashTable(u32),
    Lambda(u32),
    Macro(u32),
    ByteCode(u32),
    Overlay(u32),
    Marker(u32),
    Bignum(u32),

    Subr(u32),         // index into non-cons subr handles
    Buffer(u32),       // index into non-cons buffer handles
    Window(u32),       // index into non-cons window handles
    Frame(u32),        // index into non-cons frame handles
    Timer(u32),        // index into non-cons timer handles
}
```

Notes:

- `Nil`, `True`, `Unbound`, `Fixnum`, and `Symbol` are true immediates in the dump model.
- `Subr`, `Buffer`, `Window`, `Frame`, and `Timer` are **not** dump immediates. They are heap-backed values in the runtime, so they stay in the indexed object space and are rehydrated/canonicalized through the live heap.
- Every place that currently stores `DumpValue` in v1 gets a v2 counterpart using `DumpValueRef`.

---

## Packed Cons Slots

The cons arena stores raw 64-bit words, not archived enums. Those words use one exact encoding:

- live immediate values keep their live bit patterns
  - `nil`
  - `t`
  - `unbound`
  - fixnums
  - symbols
- cons references use the live cons tag with a cell index payload
  - `word = TAG_CONS | (cell_index << 3)`
- string references use the live string tag with a string index payload
  - `word = TAG_STRING | (string_index << 3)`
- float references use the live float tag with a float index payload
  - `word = TAG_FLOAT | (float_index << 3)`
- all veclike references use the live veclike tag plus a dump subtype and an index
  - `word = TAG_VECLIKE | ((subtype as u64) << 3) | ((index as u64) << 8)`

The dump veclike subtype namespace is:

```rust
#[repr(u8)]
enum DumpVeclikeKind {
    Vector = 0,
    Record = 1,
    HashTable = 2,
    Lambda = 3,
    Macro = 4,
    ByteCode = 5,
    Overlay = 6,
    Marker = 7,
    Bignum = 8,
    Subr = 9,
    Buffer = 10,
    Window = 11,
    Frame = 12,
    Timer = 13,
}
```

This is the only raw-slot encoding used by the new format.

### Cons Patching

The loader patches the cons arena in two passes:

1. rewrite cons-to-cons words in place from cell indices to absolute pointers
2. rewrite all string/float/veclike dump references in place to live tagged pointers using the rehydrated object tables

After pass 2, every word in every live cons cell is an ordinary runtime `TaggedValue`.

---

## Non-Cons Section

The non-cons section is one archived object graph:

```rust
#[derive(Archive, Serialize, Deserialize)]
pub struct DumpNonConsSection {
    pub strings: Vec<DumpStringV2>,
    pub floats: Vec<DumpFloatV2>,
    pub vectors: Vec<DumpVectorV2>,
    pub records: Vec<DumpVectorV2>,
    pub hash_tables: Vec<DumpHashTableV2>,
    pub lambdas: Vec<DumpLambdaV2>,
    pub macros: Vec<DumpLambdaV2>,
    pub bytecodes: Vec<DumpByteCodeV2>,
    pub overlays: Vec<DumpOverlayV2>,
    pub markers: Vec<DumpMarkerV2>,
    pub bignums: Vec<DumpBignumV2>,
    pub subrs: Vec<DumpSubrV2>,
    pub buffers: Vec<DumpBufferHandleV2>,
    pub windows: Vec<DumpWindowHandleV2>,
    pub frames: Vec<DumpFrameHandleV2>,
    pub timers: Vec<DumpTimerHandleV2>,
}
```

Rules:

- each v2 type mirrors the semantic fields currently covered by v1
- no field may be dropped just because the wire format changed
- every internal value reference inside these structs uses `DumpValueRef`

Examples:

```rust
#[derive(Archive, Serialize, Deserialize)]
pub struct DumpStringV2 {
    pub data: Vec<u8>,
    pub size: usize,
    pub size_byte: i64,
    pub text_props: Vec<DumpStringTextPropertyRunV2>,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct DumpByteCodeV2 {
    pub ops: Vec<DumpOpV2>,
    pub constants: Vec<DumpValueRef>,
    pub max_stack: u16,
    pub params: DumpLambdaParamsV2,
    pub lexical: bool,
    pub env: Option<DumpValueRef>,
    pub gnu_byte_offset_map: Option<Vec<(u32, u32)>>,
    pub docstring: Option<String>,
    pub doc_form: Option<DumpValueRef>,
    pub interactive: Option<DumpValueRef>,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct DumpHashTableV2 {
    pub test: DumpHashTableTestV2,
    pub test_name: Option<u32>, // SymId
    pub size: i64,
    pub weakness: Option<DumpHashTableWeaknessV2>,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub entries: Vec<(DumpHashKeyV2, DumpValueRef)>,
    pub key_snapshots: Vec<(DumpHashKeyV2, DumpValueRef)>,
    pub insertion_order: Vec<DumpHashKeyV2>,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct DumpSubrV2 {
    pub sym_id: u32,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct DumpBufferHandleV2 {
    pub id: u64,
}
```

Subrs and ID-backed wrappers intentionally stay minimal. The loader recreates their live heap values through the canonical runtime constructors and registries.

---

## Metadata Section

The metadata section replaces the old `DumpContextState` with a v2 metadata struct that covers the same semantic state **except** for the heap graph itself.

```rust
#[derive(Archive, Serialize, Deserialize)]
pub struct DumpMetadataV2 {
    pub interner: DumpStringInternerV2,
    pub obarray: DumpObarrayV2,
    pub dynamic: Vec<DumpOrderedSymMapV2>,
    pub lexenv: DumpValueRef,
    pub features: Vec<u32>,
    pub require_stack: Vec<u32>,
    pub loads_in_progress: Vec<String>,
    pub buffers: DumpBufferManagerV2,
    pub autoloads: DumpAutoloadManagerV2,
    pub custom: DumpCustomManagerV2,
    pub modes: DumpModeRegistryV2,
    pub coding_systems: DumpCodingSystemManagerV2,
    pub charset_registry: DumpCharsetRegistryV2,
    pub fontset_registry: DumpFontsetRegistryV2,
    pub face_table: DumpFaceTableV2,
    pub abbrevs: DumpAbbrevManagerV2,
    pub interactive: DumpInteractiveRegistryV2,
    pub rectangle: DumpRectangleStateV2,
    pub standard_syntax_table: DumpValueRef,
    pub standard_category_table: DumpValueRef,
    pub current_local_map: DumpValueRef,
    pub kmacro: DumpKmacroManagerV2,
    pub registers: DumpRegisterManagerV2,
    pub bookmarks: DumpBookmarkManagerV2,
    pub watchers: DumpVariableWatcherListV2,
}
```

Key point:

- v1 had `tagged_heap` inside `DumpContextState`
- v2 removes that field entirely
- the heap graph now lives in `cons_arena + non_cons_section`
- metadata carries only evaluator state and root references into those sections

---

## Dump Pipeline

### 1. Build Metadata First

Walk the live evaluator and produce `DumpMetadataV2`. Every heap-backed value encountered in metadata should be converted to `DumpValueRef`.

### 2. Collect Heap Graph

Traverse reachable heap values from metadata references and partition them into:

- cons cells -> cons arena
- all other heap-backed values -> typed vectors inside `DumpNonConsSection`

Deduplication tables:

- `cons_ptr_to_index: HashMap<usize, u32>`
- one index table per non-cons kind

### 3. Serialize Sections

- cons arena is emitted as raw bytes of `DumpSlotWord` pairs
- non-cons section is archived with rkyv
- metadata section is archived with rkyv

### 4. Emit RuntimeImageV2 or File

- `snapshot_evaluator` returns `RuntimeImageV2`
- `dump_to_file` writes `DumpHeader + sections`

There is no separate legacy dump path.

---

## Load Pipeline

### Shared Loader Entry

Use one internal loader over section slices:

```rust
fn load_runtime_image_v2(
    cons_backing: DumpConsBacking,
    cons_count: u64,
    non_cons_bytes: &[u8],
    metadata_bytes: &[u8],
) -> Result<Context, DumpError>
```

- file-backed load builds `DumpConsBacking::Mmap`
- in-memory restore builds `DumpConsBacking::Bytes`

### Step-by-Step Load

1. Access archived metadata.
2. Rebuild the interner from metadata before any `SymId`-dependent reconstruction.
3. Create `Box<TaggedHeap>` and install it with `set_tagged_heap(&mut heap)`.
4. Access archived non-cons section.
5. Rehydrate non-cons objects into the live heap and build one live-object table per kind:

```rust
struct LiveObjects {
    strings: Vec<Value>,
    floats: Vec<Value>,
    vectors: Vec<Value>,
    records: Vec<Value>,
    hash_tables: Vec<Value>,
    lambdas: Vec<Value>,
    macros: Vec<Value>,
    bytecodes: Vec<Value>,
    overlays: Vec<Value>,
    markers: Vec<Value>,
    bignums: Vec<Value>,
    subrs: Vec<Value>,
    buffers: Vec<Value>,
    windows: Vec<Value>,
    frames: Vec<Value>,
    timers: Vec<Value>,
}
```

6. Patch the cons arena in place:
   - cons refs -> absolute cons pointers
   - string refs -> live string pointers
   - float refs -> live float pointers
   - veclike refs -> live veclike pointers
7. Attach the cons backing store to `TaggedHeap` as `dump_cons_region`.
8. Run `reconstruct_evaluator_v2`, which handles the final evaluator assembly after heap rehydration:
   - confirm the rehydrated heap is installed as the current tagged heap
   - `reset_runtime_for_new_heap(HeapResetMode::PdumpRestore)`
   - `load_charset_registry_v2`
   - `load_fontset_registry_v2`
   - rebuild the 22 arguments to `Context::from_dump`
   - reinstall `BUFFER_OBJFWD` forwarders

There is no `finish_preload_tagged_heap()` analogue because v2 does not install a temporary `TaggedLoadState`.

### Resolving Structured Values

All metadata and non-cons fields resolve through one function:

```rust
fn resolve_dump_value_ref(
    value: &DumpValueRef,
    cons_base: usize,
    live: &LiveObjects,
) -> Value
```

Resolution rules:

- `Nil`, `True`, `Unbound`, `Fixnum`, `Symbol` -> direct constructors
- `Cons(index)` -> `unsafe { TaggedValue::from_cons_ptr(...) }`
- `String/Float/...` -> lookup in `LiveObjects`
- `Subr(index)` -> `live.subrs[index]`
- `Buffer/Window/Frame/Timer(index)` -> lookup in `LiveObjects`

### Rehydrating Each Object Kind

Use the actual runtime constructors:

- strings -> `LispString::from_dump(data, size, size_byte)` then `alloc_string`
- floats -> `alloc_float`
- vectors/records -> allocate placeholder `Vec<Value>` then fill with mutate helpers
- lambdas/macros -> allocate placeholder closure slots then fill
- bytecode -> allocate empty `ByteCodeFunction`, then populate fields
- hash tables -> allocate with options, then fill entries/key snapshots/insertion order
- overlays/markers -> allocate payload structs, then fill fields
- bignums -> `Value::make_integer_from_str_or_zero`
- subrs -> `Value::subr(SymId(sym_id))`
- buffers/windows/frames/timers -> `Value::make_buffer`, `Value::make_window`, `Value::make_frame`, `Value::make_timer`

---

## GC Integration

Only cons cells are dump-resident.

Required changes:

- `TaggedHeap` gains `dump_cons_region: Option<Box<DumpConsRegion>>`
- `DumpConsRegion` owns either `MmapMut` or `Box<[u8]>`
- `is_dump_cons(ptr)` does a pointer-range check
- mark phase traces through dump cons cells but never marks them for sweep
- sweep phase skips dump cons pointers
- conservative stack scan accepts dump-cons-region pointers

Mutation rules:

- `setcar` / `setcdr` on dump conses work via COW for mmap-backed loads and direct memory mutation for byte-backed snapshot restores
- all non-cons mutations go through the ordinary heap objects and existing `mutate.rs` helpers

---

## Public API After Cutover

These names stay:

- `pdump::dump_to_file`
- `pdump::load_from_dump`
- `pdump::snapshot_evaluator`
- `pdump::snapshot_active_evaluator`
- `pdump::restore_snapshot`
- `pdump::clone_evaluator`
- `pdump::clone_active_evaluator`

These internals go away:

- bincode payload format
- `DumpContextState` as the public snapshot type
- `DumpValue` / `DumpHeapRef`
- `TaggedLoadState`
- `preload_tagged_heap`
- `finish_preload_tagged_heap`
- version dispatch to old dump formats

---

## Performance Projection

| Phase | Current | Target |
|-------|---------|--------|
| File I/O | ~5ms read | <0.1ms mmap |
| Cons heap | ~60ms deserialize/alloc | ~2ms patch in place |
| Non-cons heap | ~50ms deserialize/alloc | ~15ms rehydrate |
| Evaluator restore | ~30ms | ~3ms |
| **Total** | **~150ms** | **~20ms** |

---

## Tasks

### Task 1: Replace Public Snapshot Type

**Files:** `neovm-core/src/emacs_core/pdump/mod.rs`

- [ ] Introduce `RuntimeImageV2`
- [ ] Change `snapshot_evaluator` / `snapshot_active_evaluator` to return `Result<RuntimeImageV2, DumpError>`
- [ ] Change `restore_snapshot` to accept `&RuntimeImageV2`
- [ ] Update clone helpers and shared tests to use the new image type
- [ ] Commit

### Task 2: Define New Format Types

**Files:** new `neovm-core/src/emacs_core/pdump/format.rs`

- [ ] Define `DumpHeader`
- [ ] Define `DumpValueRef`
- [ ] Define `DumpSlotWord` encode/decode helpers
- [ ] Define `DumpVeclikeKind`
- [ ] Define `DumpNonConsSection`
- [ ] Define every `Dump*V2` payload type
- [ ] Define `DumpMetadataV2`
- [ ] Unit tests for field coverage and slot encoding
- [ ] Commit

### Task 3: Build RuntimeImageV2

**Files:** new `neovm-core/src/emacs_core/pdump/dump.rs`

- [ ] Walk evaluator state into `DumpMetadataV2`
- [ ] Traverse reachable heap objects and partition into cons arena vs non-cons typed vectors
- [ ] Dedup conses and non-cons objects by pointer identity
- [ ] Encode cons cells into raw `DumpSlotWord` pairs
- [ ] Archive non-cons section and metadata section with rkyv
- [ ] Return `RuntimeImageV2`
- [ ] Unit tests for mixed heaps, dedup, and section completeness
- [ ] Commit

### Task 4: File Writer

**Files:** `neovm-core/src/emacs_core/pdump/mod.rs`, new `pdump/file.rs`

- [ ] Replace the old bincode file writer with `DumpHeader + sections`
- [ ] Compute checksum over all sections after the header
- [ ] Write atomically via temp file as today
- [ ] Unit tests for header bounds and checksum validation
- [ ] Commit

### Task 5: Loader Core

**Files:** new `neovm-core/src/emacs_core/pdump/load.rs`

- [ ] Implement shared `load_runtime_image_v2`
- [ ] Implement file-backed section extraction from `MmapMut`
- [ ] Implement byte-backed section extraction from `RuntimeImageV2`
- [ ] Rebuild interner before object rehydration
- [ ] Rehydrate all non-cons kinds into `LiveObjects`
- [ ] Patch cons arena in two passes
- [ ] Attach `DumpConsBacking` to `TaggedHeap`
- [ ] Commit

### Task 6: Evaluator Reconstruction

**Files:** `pdump/load.rs`, `pdump/mod.rs`

- [ ] Implement `reconstruct_evaluator_v2`
- [ ] Mirror the current post-heap orchestration: confirm heap install, reset runtime caches, restore charset/fontset registries, call `Context::from_dump`, then reinstall `BUFFER_OBJFWD`
- [ ] Rebuild all 22 `Context::from_dump` arguments from `DumpMetadataV2`
- [ ] Remove v1-only preload helpers
- [ ] Unit tests for `restore_snapshot`, `clone_evaluator`, and `clone_active_evaluator`
- [ ] Commit

### Task 7: GC Support for Dump Cons Backing

**Files:** `neovm-core/src/tagged/gc.rs`

- [ ] Add `DumpConsBacking`
- [ ] Add `DumpConsRegion`
- [ ] Implement `is_dump_cons`
- [ ] Trace through dump conses during mark
- [ ] Skip dump conses during sweep
- [ ] Accept dump cons pointers in conservative scan
- [ ] Unit tests for file-backed and byte-backed dump cons regions
- [ ] Commit

### Task 8: Bootstrap Integration

**Files:** `neovm-core/src/emacs_core/load.rs`, `xtask/src/main.rs`

- [ ] Keep the public names `dump_to_file` and `load_from_dump`
- [ ] Replace their implementations with the new format
- [ ] Remove any old-format assumptions from bootstrap code
- [ ] Full bootstrap round-trip with the new format only
- [ ] Benchmark release startup
- [ ] Commit

### Task 9: Delete the Old Pipeline

**Files:** `pdump/types.rs`, `pdump/convert.rs`, `pdump/mod.rs`, related tests

- [ ] Delete `DumpContextState`, `DumpValue`, `DumpHeapRef`, and `TaggedLoadState`
- [ ] Delete bincode serialization/deserialization code
- [ ] Delete old-format tests
- [ ] Rename any temporary `*V2` module/type names if desired
- [ ] Commit

---

## Risks

1. **Slot encoding drift:** `DumpSlotWord` is the most fragile part of the design. Keep one exact encoding and test it exhaustively.
2. **Field coverage drift:** every v2 payload type must be diffed against the current runtime/v1 semantic surface before code lands.
3. **Snapshot lifetime:** dump cons backing must survive for the lifetime of the restored `Context` for both mmap-backed and byte-backed loads.
4. **ID-backed wrappers:** subrs, buffers, windows, frames, and timers must be canonicalized through the live heap so repeated references preserve runtime identity semantics.
5. **Release pdump crash:** the current release-mode pdump SIGSEGV is pre-existing and may need to be solved in parallel.
6. **No compatibility path:** this is intentional, but it means the branch must keep bootstrapping as the refactor lands.
