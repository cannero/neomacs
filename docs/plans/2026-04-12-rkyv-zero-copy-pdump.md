# rkyv Zero-Copy Pdump for NeoMacs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current bincode/serde pdump with mmap + zero-copy deserialization to bring TTY startup from ~150ms (release) to ~20ms.

**Architecture:** Two-arena dump. Cons cells are mmap'd directly (zero-copy). All other heap objects are serialized via rkyv and rehydrated into live Rust heap objects on load. The existing `reconstruct_evaluator` orchestrator (pdump/mod.rs:231) and `Context::from_dump` (eval.rs:3819, 22-arg entry point) are reused for final evaluator assembly.

**Tech Stack:** rkyv 0.8, memmap2, bytemuck

---

## Current State

NeoMacs pdump v1 (`pdump/mod.rs`, `pdump/types.rs`, `pdump/convert.rs`):

- **Dump types**: `DumpValue` enum (types.rs:29) uses `DumpHeapRef(u32)` indices to reference heap objects. NOT offset-tagged pointers.
- **Dump entry**: `dump_evaluator()` walks the heap, builds `DumpContextState` (types.rs:1032, ~25 fields), serializes via bincode.
- **Load entry**: `restore_snapshot(state: &DumpContextState)` (mod.rs:210) calls `reconstruct_evaluator(state)` (mod.rs:231).
- **Reconstruct**: Two-pass via `TaggedLoadState` (convert.rs:102). Pass 1: `allocate_tagged_placeholder` (convert.rs:1565) creates live objects with NIL slots. Pass 2: resolves DumpValue heap-ref indices to live TaggedValues and fills slots.
- **Context assembly**: `Context::from_dump(...)` (eval.rs:3819) takes **22 individual arguments** (heap, obarray, lexenv, features, require_stack, loads_in_progress, buffers, autoloads, custom, modes, coding_systems, face_table, abbrevs, interactive, rectangle, standard_syntax_table, standard_category_table, current_local_map, kmacro, registers, bookmarks, watchers). The `reconstruct_evaluator` function unpacks `DumpContextState` fields and calls this.

**Performance**: ~150ms release, ~700ms debug. GNU Emacs: ~30ms.

---

## v2 Design

### Two Arenas

**Cons arena (mmap'd, zero-copy):** Flat array of 16-byte `ConsCell` values. No `GcHeader` — cons cells use external mark bitmap in the `ConsBlock` allocator (tagged/header.rs:71). After relocating pointer slots (rewriting arena offsets to absolute mmap'd addresses), these bytes ARE valid live ConsCells. Cons cells are ~80% of objects by count.

**Non-cons section (rkyv-serialized, rehydrated on load):** All other heap types (StringObj, FloatObj, VectorObj, HashTableObj, LambdaObj, ByteCodeObj, etc.) contain Rust heap containers (Vec, HashMap, OnceLock) in their live layouts and CANNOT be mmap'd. They are serialized via rkyv `#[derive(Archive, Serialize)]` on NEW v2 dump types (see below) and rehydrated into live Rust heap objects on load using TaggedHeap allocation APIs.

### Why NOT Reuse pdump/types.rs Dump* Types

The v1 Dump* types use `DumpValue::HeapRef(u32)` for inter-object references. The v2 format uses `DumpTaggedValue` (offset-based tagged pointers into the cons arena, plus rkyv-internal relative pointers for non-cons objects). These are fundamentally different reference models. The v1 `convert.rs` restore functions (`allocate_tagged_placeholder`, `load_value`, etc.) resolve HeapRef indices through a `TaggedLoadState` index table — this logic does not apply to the v2 format.

Therefore: **v2 defines NEW dump types** with rkyv derives and `DumpTaggedValue` references. The v1 types and convert.rs are NOT reused. However, the field LISTS of the new types must cover every field that v1 covers (text_props, gnu_byte_offset_map, key_snapshots, insertion_order, parameter names, etc.) — verified by diffing against v1 types.

The v2 restore functions are new code, but `reconstruct_evaluator` (the orchestrator that unpacks fields and calls `Context::from_dump` with 22 args) is reused with minimal changes (swap DumpValue resolution for DumpTaggedValue resolution).

---

## File Layout

```
struct DumpHeaderV2 {                     // 128 bytes total
    magic: [u8; 8],                       // b"NEODUMP2"
    version: u32,
    _pad0: u32,
    cons_arena_offset: u64,
    cons_arena_len: u64,                  // byte count
    cons_count: u64,                      // number of cons cells
    non_cons_offset: u64,                 // rkyv-serialized non-cons section
    non_cons_len: u64,
    metadata_offset: u64,                 // rkyv-serialized DumpContextStateV2
    metadata_len: u64,
    roots_offset: u64,                    // array of DumpTaggedValue
    roots_count: u64,
    checksum: [u8; 32],                   // SHA-256 of everything after header
}
```

Sections in file order: header (128B) | cons arena | non-cons section | metadata | roots.

No separate object table section. The cons arena is a flat array of `cons_count` ConsCells (16 bytes each) — the loader walks them all during relocation. Non-cons objects are accessed via rkyv's built-in traversal (Archive derive handles offsets internally).

---

## DumpTaggedValue

```rust
/// 8 bytes. Same tag encoding as TaggedValue (value.rs:89).
/// Immediates (fixnum, symbol, nil, t, unbound) are bit-identical
/// to live TaggedValues. Cons pointers store a byte offset into
/// the cons arena (NOT an absolute address). Non-cons heap
/// pointers store an rkyv-internal relative pointer index
/// (resolved by the rkyv deserializer during rehydration).
#[repr(transparent)]
#[derive(Clone, Copy, rkyv::Archive, rkyv::Serialize)]
pub struct DumpTaggedValue(u64);
```

---

## Cons Arena Layout

```rust
/// Identical to live ConsCell (tagged/header.rs:71).
/// [car: DumpTaggedValue (8B)] [cdr: DumpTaggedValue (8B)]
/// No GcHeader. After relocation, these bytes are live ConsCells.
```

Relocation rewrites every cons cell's car and cdr in place:

```rust
fn relocate_cons_arena(arena: &mut [u8], base_addr: usize, count: usize) {
    for i in 0..count {
        let cell = unsafe {
            &mut *(arena.as_mut_ptr().add(i * 16) as *mut [u64; 2])
        };
        relocate_slot(&mut cell[0], base_addr); // car
        relocate_slot(&mut cell[1], base_addr); // cdr
    }
}

fn relocate_slot(slot: &mut u64, cons_base: usize) {
    let tag = *slot & TAG_MASK;
    if tag == TAG_CONS as u64 {
        // Cons pointer: arena offset -> absolute mmap'd address
        let offset = (*slot >> 3) as usize;
        let abs = cons_base + offset * 16; // offset is cell index
        *slot = (abs as u64) | tag;
    }
    // Non-cons heap pointers are NOT in the cons arena.
    // They are resolved during non-cons rehydration.
    // Immediates (fixnum, symbol, nil, t) pass through unchanged.
}
```

After relocation, cons pointers in the cons arena point to other
mmap'd cons cells via absolute addresses. Non-cons pointers in
cons cells (e.g., car pointing to a string) are still unresolved
arena-offset values — they are patched in a second pass after
non-cons rehydration assigns live heap addresses.

---

## Non-Cons Section

rkyv-serialized Vec of v2 dump objects:

```rust
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DumpNonConsSection {
    pub strings: Vec<DumpStringV2>,
    pub floats: Vec<f64>,
    pub vectors: Vec<DumpVectorV2>,
    pub records: Vec<DumpVectorV2>,
    pub hash_tables: Vec<DumpHashTableV2>,
    pub lambdas: Vec<DumpLambdaV2>,
    pub macros: Vec<DumpLambdaV2>,
    pub bytecodes: Vec<DumpByteCodeV2>,
    pub overlays: Vec<DumpOverlayV2>,
    pub markers: Vec<DumpMarkerV2>,
    pub bignums: Vec<String>,
}
```

Each v2 dump type mirrors ALL fields from the v1 types.rs
equivalent. Example:

```rust
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DumpStringV2 {
    pub data: Vec<u8>,
    pub size_byte: i64,
    pub text_props: DumpTextPropertyTableV2,  // NOT omitted
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DumpByteCodeV2 {
    pub ops: Vec<DumpOp>,
    pub constants: Vec<DumpTaggedValue>,
    pub max_stack: u16,
    pub params: DumpLambdaParams,
    pub lexical: bool,
    pub env: Option<DumpTaggedValue>,
    pub gnu_byte_offset_map: Option<Vec<(u32, u32)>>,  // NOT omitted
    pub docstring: Option<String>,                       // NOT omitted
    pub doc_form: Option<DumpTaggedValue>,
    pub interactive: Option<DumpTaggedValue>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DumpHashTableV2 {
    pub test: DumpHashTableTest,
    pub test_name: Option<DumpSymId>,           // NOT omitted
    pub size: i64,                               // NOT omitted
    pub weakness: Option<DumpHashTableWeakness>,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub entries: Vec<(DumpHashKeyV2, DumpTaggedValue)>,
    pub key_snapshots: Vec<(DumpHashKeyV2, DumpTaggedValue)>,  // NOT omitted
    pub insertion_order: Vec<DumpHashKeyV2>,                     // NOT omitted
}
```

---

## Metadata Section

rkyv-serialized `DumpContextStateV2`, mirroring all ~25 fields
of v1's `DumpContextState` (types.rs:1032). Uses
`DumpTaggedValue` where v1 uses `DumpValue`. The field list
matches v1 exactly:

```rust
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DumpContextStateV2 {
    pub interner: DumpStringInterner,      // same as v1
    pub obarray: DumpObarrayV2,            // uses DumpTaggedValue
    pub dynamic: Vec<DumpOrderedSymMapV2>,
    pub lexenv: DumpTaggedValue,
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
    pub standard_syntax_table: DumpTaggedValue,
    pub standard_category_table: DumpTaggedValue,
    pub current_local_map: DumpTaggedValue,
    pub kmacro: DumpKmacroManagerV2,
    pub registers: DumpRegisterManagerV2,
    pub bookmarks: DumpBookmarkManagerV2,
    pub watchers: DumpVariableWatcherListV2,
}
```

---

## Load Process

```rust
pub fn load_dump_v2(path: &Path) -> Result<Context, DumpError> {
    // 1. mmap with MAP_PRIVATE (copy-on-write for cons mutations)
    let file = std::fs::File::open(path)?;
    let mut mmap = unsafe { memmap2::MmapMut::map_copy(&file)? };

    // 2. Validate 128-byte header
    let header: &DumpHeaderV2 = // read from &mmap[..128]
    header.validate_magic_and_version()?;

    // 3. Relocate cons arena in place (~2ms)
    let cons_arena = &mut mmap[
        header.cons_arena_offset as usize
        .. header.cons_arena_offset as usize + header.cons_arena_len as usize
    ];
    let cons_base = cons_arena.as_ptr() as usize;
    relocate_cons_arena(cons_arena, cons_base, header.cons_count as usize);

    // 4. Access non-cons section via rkyv zero-copy
    let non_cons_bytes = &mmap[
        header.non_cons_offset as usize
        .. header.non_cons_offset as usize + header.non_cons_len as usize
    ];
    let archived_non_cons = rkyv::access::<
        ArchivedDumpNonConsSection, rkyv::rancor::Error
    >(non_cons_bytes)?;

    // 5. Rehydrate non-cons objects (~15ms)
    //    Allocates live Rust heap objects using TaggedHeap APIs:
    //      alloc_string(LispString)         — gc.rs:779
    //      alloc_float(f64)                 — gc.rs:793
    //      alloc_vector(Vec<TaggedValue>)   — gc.rs:837
    //      alloc_lambda(Vec<TaggedValue>)   — gc.rs:871
    //    Returns a map: (object_type, index) -> live TaggedValue
    let mut heap = Box::new(TaggedHeap::new());
    let live_map = rehydrate_non_cons(&archived_non_cons, cons_base, &mut heap);

    // 6. Patch cons arena: fix non-cons pointers
    //    After step 3, cons cells with car/cdr pointing to non-cons
    //    objects still have unresolved values. Walk cons arena again
    //    and resolve non-cons DumpTaggedValues via live_map.
    patch_cons_non_cons_refs(cons_arena, cons_base, &live_map);

    // 7. Set up dump-cons-aware heap
    heap.set_dump_cons_region(DumpConsRegion {
        base: cons_base as *const u8,
        len: header.cons_arena_len as usize,
        _mmap: mmap,  // heap owns mmap to keep it alive
    });

    // 8. Access metadata via rkyv zero-copy
    let archived_meta = rkyv::access::<
        ArchivedDumpContextStateV2, rkyv::rancor::Error
    >(&mmap[header.metadata_offset as usize ..])?;

    // 9. Build live evaluator state from archived metadata.
    //    Resolve DumpTaggedValue fields via live_map + cons_base.
    //    Unpack into the 22 individual arguments that
    //    Context::from_dump (eval.rs:3819) expects:
    let ctx = Context::from_dump(
        heap,                                              // Box<TaggedHeap>
        rebuild_obarray(&archived_meta.obarray, ...),      // Obarray
        resolve_dtv(archived_meta.lexenv, ...),             // Value (lexenv)
        rebuild_features(&archived_meta.features),          // Vec<SymId>
        rebuild_require_stack(&archived_meta.require_stack), // Vec<SymId>
        rebuild_loads(&archived_meta.loads_in_progress),    // Vec<PathBuf>
        rebuild_buffers(&archived_meta.buffers, ...),       // BufferManager
        rebuild_autoloads(&archived_meta.autoloads, ...),   // AutoloadManager
        rebuild_custom(&archived_meta.custom, ...),         // CustomManager
        rebuild_modes(&archived_meta.modes, ...),           // ModeRegistry
        rebuild_coding(&archived_meta.coding_systems, ...),
        rebuild_face_table(&archived_meta.face_table, ...),
        rebuild_abbrevs(&archived_meta.abbrevs, ...),
        rebuild_interactive(&archived_meta.interactive, ...),
        rebuild_rectangle(&archived_meta.rectangle, ...),
        resolve_dtv(archived_meta.standard_syntax_table, ...),
        resolve_dtv(archived_meta.standard_category_table, ...),
        resolve_dtv(archived_meta.current_local_map, ...),
        rebuild_kmacro(&archived_meta.kmacro, ...),
        rebuild_registers(&archived_meta.registers, ...),
        rebuild_bookmarks(&archived_meta.bookmarks, ...),
        rebuild_watchers(&archived_meta.watchers, ...),
    );
    Ok(ctx)
}
```

---

## GC Integration

Only cons cells are dump-resident. All other objects are normal
heap allocations from rehydration.

**Detection**: `TaggedHeap::is_dump_cons(ptr: *const ConsCell) -> bool` — O(1) pointer-range check against `dump_cons_region`.

**Mark phase**: When tracing a cons, check `is_dump_cons`. If yes, do NOT mark it (always live). But DO trace car and cdr — they may reference rehydrated heap objects that need marking.

**Sweep phase**: Skip dump-resident cons pointers (outside ConsBlock allocator). All non-cons objects are normal — sweep unchanged.

**Conservative stack scan**: `is_valid_heap_pointer` accepts dump-cons-region pointers.

**Mutation**: MAP_PRIVATE gives transparent COW. setcar/setcdr on dump conses works at the memory level. The GC traces all reachable conses anyway. All 17 mutation paths in mutate.rs target rehydrated (normal heap) objects and already have write tracking — no changes needed.

---

## Performance Projection

| Phase | v1 (bincode) | v2 (rkyv) |
|-------|-------------|-----------|
| File I/O | 5ms (read) | <0.1ms (mmap) |
| Cons cells (~80K) | ~60ms (alloc) | ~2ms (relocate) |
| Non-cons (~20K) | ~50ms (alloc) | ~15ms (rehydrate) |
| Evaluator assembly | ~30ms | ~3ms |
| **Total** | **~150ms** | **~20ms** |

---

## Tasks

### Task 1: Dependencies and DumpTaggedValue

**Files:** `neovm-core/Cargo.toml`, new `pdump/v2_types.rs`

- [ ] Add `memmap2 = "0.9"`, `rkyv = { version = "0.8", features = ["bytecheck"] }` to Cargo.toml
- [ ] Define `DumpTaggedValue(u64)` with tag helpers matching TaggedValue (value.rs:89)
- [ ] Define `DumpHeaderV2` (128 bytes)
- [ ] Unit tests: DumpTaggedValue round-trip for fixnum, symbol, cons offset, nil, t
- [ ] Commit

### Task 2: v2 Dump Types with rkyv Derives

**Files:** new `pdump/v2_types.rs`

- [ ] Define `DumpStringV2`, `DumpByteCodeV2`, `DumpHashTableV2`, `DumpLambdaV2`, `DumpOverlayV2`, `DumpMarkerV2`, `DumpVectorV2` — all with `#[derive(Archive, Serialize, Deserialize)]`, all fields matching v1 types.rs equivalents (diff against types.rs to verify completeness)
- [ ] Define `DumpNonConsSection` containing Vec of each type
- [ ] Define `DumpContextStateV2` with all ~25 fields matching v1's DumpContextState
- [ ] Unit tests: each v2 type serializes and archives correctly
- [ ] Commit

### Task 3: Cons Arena Builder

**Files:** new `pdump/v2_dump.rs`

- [ ] Implement cons arena builder: depth-first walk from roots, dedup via `ptr_to_offset: HashMap`, write 16-byte ConsCells with DumpTaggedValue car/cdr
- [ ] DumpTaggedValue for cons pointers stores cell INDEX (not byte offset) for compact encoding
- [ ] DumpTaggedValue for non-cons pointers stores (type, index) pair identifying the object in DumpNonConsSection's typed Vecs
- [ ] Unit tests: dump a cons tree, verify arena byte layout
- [ ] Commit

### Task 4: Non-Cons and Metadata Serialization

**Files:** `pdump/v2_dump.rs`

- [ ] Implement non-cons collector: walk all reachable non-cons objects, populate DumpNonConsSection's typed Vecs
- [ ] Implement metadata collector: populate DumpContextStateV2 from live Context (mirror v1's dump_evaluator logic)
- [ ] Implement `dump_to_file_v2()`: write header + cons arena + rkyv-serialized non-cons + rkyv-serialized metadata + roots
- [ ] Unit tests: dump a bootstrap Context, verify header, verify all sections present
- [ ] Commit

### Task 5: Cons Arena Loader with Relocation

**Files:** new `pdump/v2_load.rs`

- [ ] Implement `load_dump_v2()`: mmap file, validate header, extract section slices
- [ ] Implement `relocate_cons_arena()`: walk cons cells, rewrite cons-pointer slots from cell indices to absolute mmap'd addresses via `unsafe TaggedValue::from_cons_ptr()` (value.rs:165)
- [ ] Unit tests: dump + relocate round-trip, verify cons car/cdr are valid pointers
- [ ] Commit

### Task 6: Non-Cons Rehydration

**Files:** `pdump/v2_load.rs`

- [ ] Access archived non-cons section via rkyv zero-copy
- [ ] Rehydrate each object type using actual TaggedHeap APIs: `alloc_string(LispString)` (gc.rs:779), `alloc_float(f64)` (gc.rs:793), `alloc_vector(Vec<TaggedValue>)` (gc.rs:837), `alloc_lambda(Vec<TaggedValue>)` (gc.rs:871), etc.
- [ ] Two-pass: pass 1 allocates with NIL slots, pass 2 resolves DumpTaggedValue references via cons_base + live_map
- [ ] Patch cons arena: walk cons cells again, resolve car/cdr that point to non-cons objects via live_map
- [ ] Unit tests: dump + load round-trip with mixed cons/string/vector heap
- [ ] Commit

### Task 7: Evaluator State Restore

**Files:** `pdump/v2_load.rs`, modify `pdump/mod.rs`

- [ ] Implement `rebuild_*` functions for each of the 22 args to Context::from_dump (eval.rs:3819): obarray, buffers, autoloads, modes, coding_systems, face_table, abbrevs, interactive, rectangle, kmacro, registers, bookmarks, watchers, etc.
- [ ] Each rebuild function accesses archived DumpContextStateV2 fields via rkyv zero-copy, resolves DumpTaggedValues, constructs live Rust types
- [ ] Call `Context::from_dump(heap, obarray, lexenv, ...)` with all 22 args
- [ ] Wire into `restore_snapshot` with FORMAT_VERSION dispatch (v1 bincode, v2 rkyv)
- [ ] Unit tests: dump + load round-trip, verify `(+ 1 2)` evals, `(car '(a b))` works, `(symbol-value 'features)` returns correct list
- [ ] Commit

### Task 8: DumpConsRegion in TaggedHeap

**Files:** modify `tagged/gc.rs`

- [ ] Add `dump_cons_region: Option<Box<DumpConsRegion>>` to TaggedHeap
- [ ] `is_dump_cons(ptr)` — O(1) pointer-range check
- [ ] GC mark: trace through dump conses (do NOT mark, DO trace car/cdr)
- [ ] GC sweep: skip dump cons pointers
- [ ] Conservative scan: accept dump-cons-region pointers
- [ ] Unit tests: GC with dump + heap cons mix
- [ ] Commit

### Task 9: Bootstrap Integration

**Files:** `pdump/mod.rs`, `xtask/src/main.rs`

- [ ] Wire `dump_to_file_v2` into xtask bootstrap pipeline
- [ ] Wire `load_dump_v2` into `load_runtime_image` with version dispatch
- [ ] Full bootstrap: temacs -> pdump v2 -> load -> verify
- [ ] Benchmark startup time (release build, `-nw -Q --batch`)
- [ ] Commit

---

## Risks

1. **Alignment**: Cons arena cells must be 16-byte aligned (natural for 2x u64). Mmap guarantees page alignment (4KB), so the first cell is always aligned.
2. **Versioning**: FORMAT_VERSION in header. Old dumps rejected; regenerate from bootstrap.
3. **v1 type field coverage**: v2 types MUST cover every field v1 covers. Verified by diffing v2 type fields against v1 types.rs. Missing a field = silent data loss.
4. **Release pdump SIGSEGV**: Pre-existing bug. Must be fixed independently.
5. **Two-pass cons patching**: Cons cells may reference non-cons objects that aren't rehydrated yet during cons relocation. The patch pass (step 6 in loader) fixes this after rehydration.
6. **mmap lifetime**: DumpConsRegion in TaggedHeap owns the MmapMut. Never dropped until process exit.
7. **Endianness**: Native byte order only. No cross-architecture dump sharing.
