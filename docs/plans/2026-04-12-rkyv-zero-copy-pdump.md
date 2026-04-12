# rkyv Zero-Copy Pdump for NeoMacs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current bincode/serde pdump with mmap + zero-copy deserialization to bring startup from ~150ms (release) to <5ms, matching or beating GNU Emacs's ~30ms.

**Architecture:** Hybrid custom-arena + rkyv. Hot Lisp heap objects (cons, string, float, vector, bytecode) live in a flat mmap'd arena with offset-based pointers that are eagerly relocated to absolute pointers on load (~2ms). Cold metadata (interner, obarray, buffer manager) is serialized via rkyv derive macros and accessed zero-copy from the same mmap.

**Tech Stack:** rkyv 0.8, memmap2, bytemuck (for repr(C) casting)

---

## Background

### Current State

NeoMacs startup (release build, `-nw -Q --batch`): ~150ms.
GNU Emacs startup (same flags): ~30ms.

The current pdump (`neovm-core/src/emacs_core/pdump/`) uses bincode + serde:

1. `dump_evaluator()` walks the Lisp heap, serializes every object via `#[derive(Serialize)]`
2. `load_runtime_image()` reads the 13MB file, deserializes via bincode, allocates 100K+ Rust heap objects, rebuilds the TaggedHeap

Each object is individually allocated on the Rust heap during deserialization. This is inherently O(N) in allocations where N = number of Lisp objects.

### GNU Emacs Approach

GNU's `pdumper.c` (Daniel Colascione, 2018):

1. Dumps the C heap into a flat file with a relocation table
2. On startup: `mmap()` the file, apply relocations (pointer fixups) in-place
3. All Lisp objects are immediately live — no deserialization

Cost: mmap syscall (<0.1ms) + relocation pass (~15ms for 40MB dump).

### rkyv Approach

rkyv (archive) is a Rust zero-copy deserialization framework. Archived data uses relative pointers (offsets from the pointer's own location), so mmap'd data can be accessed at any address without relocation. NeoMacs can use a hybrid: rkyv for structured metadata, custom arena for the hot Lisp heap with GNU-style offset-based tagged values and eager relocation.

---

## Dump File Layout

```
Offset  Section
------  -------
0x0000  Header (128 bytes)
        struct DumpHeader {
            magic: [u8; 8],                // b"NEODUMP2"
            version: u32,                  // FORMAT_VERSION
            _pad0: u32,
            cons_arena_offset: u64,        // byte offset of cons arena
            cons_arena_len: u64,           // byte length of cons arena
            non_cons_arena_offset: u64,    // byte offset of non-cons arena
            non_cons_arena_len: u64,       // byte length of non-cons arena
            object_table_offset: u64,      // byte offset of object table
            object_table_count: u64,       // number of ObjectTableEntry items
            metadata_offset: u64,          // byte offset of rkyv metadata
            metadata_len: u64,             // byte length of rkyv metadata
            roots_offset: u64,             // byte offset of root table
            roots_count: u64,              // number of DumpTaggedValue roots
            checksum: [u8; 32],            // SHA-256 of all sections after header
        }  // total: 128 bytes

0x0080  Cons Arena (hot section, true zero-copy)
        Flat byte buffer of ConsCell objects (no GcHeader).
        Each cell: 16 bytes, 8-byte aligned.
        Pointer slots store DumpTaggedValue (offset-based).
        Cons cells use external mark bitmap (ConsBlock allocator)
        and have NO GcHeader prefix — mmap'd cells are directly
        usable by the runtime after pointer relocation.

0xNNNN  Non-Cons Object Arena (rehydrated on load)
        Flat byte buffer of non-cons heap objects in an inline
        format. These objects contain Rust-heap containers
        (Vec, HashMap, OnceLock) in their live layouts, so they
        CANNOT be used directly from mmap. Instead, each object
        is stored in a dump-specific inline format:

        The inline format for each type mirrors the existing Dump*
        types in pdump/types.rs (DumpString, DumpByteCodeFunction,
        DumpHashTable, etc.) which already capture ALL fields
        including text_props, parameter names, gnu_byte_offset_map,
        key_snapshots, insertion_order, etc. The v2 format reuses
        these same type definitions — the only change is the
        serialization codec (rkyv instead of bincode).

        On load, the loader deserializes each non-cons object via
        rkyv zero-copy access, then allocates live Rust heap objects
        (with GcHeader, Vec, HashMap, etc.) and populates them from
        the archived data using the existing pdump/convert.rs
        restore functions. This is ~15-20ms for ~20K objects.

0xOOOO  Object Table
        Array of entries describing every object in both arenas:
        struct ObjectTableEntry {
            kind: DumpObjectKind, // unified enum (see below), u8
            _pad: u8,
            offset: u32,     // byte offset within the object's arena
            slot_count: u16, // number of DumpTaggedValue pointer slots
        }  // 8 bytes per entry

        DumpObjectKind is a UNIFIED enum that avoids the numeric
        overlap between HeapObjectKind(0,1,2) and VecLikeType(0..13).
        The arena field is implicit from the kind: Cons objects are
        in the cons arena, everything else is in the non-cons arena.

        #[repr(u8)]
        enum DumpObjectKind {
            // Cons arena (no GcHeader, external mark bitmap)
            Cons = 0,
            // Non-cons arena (rehydrated into live heap objects)
            String = 1,
            Float = 2,
            Vector = 3,
            HashTable = 4,
            Lambda = 5,
            Macro = 6,
            ByteCode = 7,
            Record = 8,
            Overlay = 9,
            Marker = 10,
            Subr = 11,
            Bignum = 12,
        }

        The relocation pass walks entries where kind==Cons. The
        rehydration pass walks all other entries.

0xPPPP  Metadata (cold section, rkyv-serialized)
        ArchivedDumpMetadata containing:
        - interner_strings: Vec<String>
        - obarray_symbols: Vec<ArchivedSymbolData>
        - features: Vec<u32>
        - buffer_manager state
        - autoload manager state
        - mode/coding/charset registries

0xQQQQ  Root Table
        Array of DumpTaggedValues for top-level roots
        (global variables, special forms, etc.)
```

### Why Two Arenas: Zero-Copy vs Rehydration

The live heap uses two different storage strategies:

**Cons cells**: 16 bytes (`car: TaggedValue + cdr: TaggedValue`), NO
GcHeader, external mark bitmap in ConsBlock allocator. These can be
mmap'd directly — after relocating the pointer slots, the mmap'd
bytes ARE valid ConsCells. This is the biggest win: cons cells are
~80% of the dump by object count.

**All other types** (StringObj, FloatObj, VecLikeHeader-based types):
Live layouts include `GcHeader` (16 bytes: marked + kind + next
pointer) at offset 0, plus Rust heap containers like `Vec<T>`,
`HashMap`, `OnceLock`, `String`. These containers store data via
absolute heap pointers from the dumping process. A Vec is 24 bytes
(pointer + len + capacity) — the pointer is meaningless after mmap.

These objects CANNOT be used directly from mmap. They must be
"rehydrated": allocate a live Rust object, pre-fill GcHeader, copy
data from the dump's inline format into Vec/HashMap/etc. This is
more expensive than cons relocation but still much faster than full
serde deserialization because the dump stores data in a compact
inline format (no field names, no type tags, no variable-length
encoding).

```
                     Live Layout                Dump Layout
                     ===========                ===========
ConsCell:            [car|cdr] 16B              [car|cdr] 16B  (IDENTICAL)
                     no GcHeader                no GcHeader

FloatObj:            [GcHeader|f64] 24B         [f64] 8B
                     GcHeader has next ptr       rehydrated on load

VectorObj:           [GcHeader|VecLikeHeader    [len|slot0|slot1|...] inline
                      |Vec<TaggedValue>] 48B+    rehydrated into Vec on load
                     Vec has heap pointer

StringObj:           [GcHeader|LispString       [size_byte|len|bytes...] inline
                      |TextPropTable] 56B+       rehydrated into Vec<u8> on load
                     LispString.data = Vec<u8>
```

---

## DumpTaggedValue Encoding

Same 8-byte layout as live TaggedValue. Same tag bits. But heap
"pointers" are byte offsets into the arena instead of absolute
addresses:

```rust
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct DumpTaggedValue(u64);

// Tag encoding (matches TaggedValue):
//   Fixnum:  2-bit tag + 62-bit signed integer (pass-through)
//   Symbol:  3-bit tag + 32-bit SymId (pass-through)
//   Nil/T:   sentinel values (pass-through)
//   Cons:    3-bit tag + 61-bit arena byte offset
//   String:  3-bit tag + 61-bit arena byte offset
//   Float:   3-bit tag + 61-bit arena byte offset
//   VecLike: 3-bit tag + 61-bit arena byte offset
```

On load, the relocation pass rewrites every offset to an absolute
pointer by adding the arena's mmap base address.

---

## Archived Object Layouts

### Cons Arena: Matches Live Layout Exactly

```rust
/// 16 bytes — IDENTICAL to live ConsCell (header.rs:71)
/// NO GcHeader. Cons cells use external mark bitmap in ConsBlock.
/// After relocation, these bytes ARE valid live ConsCells.
#[repr(C)]
pub struct DumpConsCell {
    pub car: DumpTaggedValue,           // 8 bytes, offset 0
    pub cdr: DumpTaggedValue,           // 8 bytes, offset 8
}
// The ConsCdrOrNext union has the same size as TaggedValue.
// For dump-resident conses, the cdr field is always the cdr
// value (not a free-list pointer), so no union is needed.
```

### Non-Cons Arena: rkyv-Serialized Existing Dump Types

The non-cons arena serializes objects using the SAME type
definitions that the current v1 pdump already uses in
`pdump/types.rs`. These types already capture ALL fields of
every object type, including:

- `DumpString` (types.rs:79): data bytes, size, size_byte,
  multibyte flag, AND `text_props` (TextPropertyTable)
- `DumpByteCodeFunction` (types.rs:117): ops, constants, env,
  doc_form, interactive, parameter names, max_stack,
  `gnu_byte_offset_map`, full `LambdaParams`
- `DumpHashTable` (types.rs:269): test_name, size, entries with
  `HashKeys`, weakness, rehash_size, rehash_threshold,
  `key_snapshots`, `insertion_order`
- `DumpLambda/DumpMacro`: closure slot Vec + parsed_params
- `DumpOverlay` (types.rs): plist, buffer_id, start, end, flags
- `DumpMarker` (types.rs): buffer_id, position, insertion_type

The v2 change is ONLY the serialization codec: rkyv
`#[derive(Archive, Serialize)]` instead of serde
`#[derive(Serialize, Deserialize)]`. The type definitions,
field lists, and restore logic in `pdump/convert.rs` remain
unchanged.

On load, the archived data is accessed zero-copy via rkyv, then
the existing `convert.rs` restore functions allocate live Rust
heap objects (with GcHeader, Vec, HashMap, etc.) and populate
them. The TaggedHeap APIs used are the actual ones:

- `TaggedHeap::alloc_string(LispString)` (gc.rs:779)
- `TaggedHeap::alloc_float(f64)` (gc.rs:837)
- `TaggedHeap::alloc_lambda(Vec<TaggedValue>)` (gc.rs:895)
- `TaggedHeap::alloc_vector(Vec<TaggedValue>)` (gc.rs:986)
- `TaggedHeap::alloc_record(Vec<TaggedValue>)` (gc.rs similar)
- `TaggedHeap::alloc_hash_table(LispHashTable)`
- `TaggedHeap::alloc_bytecode(ByteCodeFunction)`
- `TaggedHeap::alloc_overlay(OverlayData)`
- `TaggedHeap::alloc_marker(MarkerData)`

DumpTaggedValue slots in archived objects are resolved to live
TaggedValues via `resolve_dump_value()` which maps cons offsets
to mmap'd addresses and non-cons offsets to rehydrated heap
pointers. Mutation of live objects uses existing `mutate.rs`
helpers (e.g., `set_vector_slot` at mutate.rs:61, not
`heap.set_vector_slot`).

---

## Load Process

```rust
pub fn load_dump_v2(path: &Path) -> Result<Context, DumpError> {
    // 1. mmap the file with MAP_PRIVATE (copy-on-write for cons mutations)
    let file = std::fs::File::open(path)?;
    let mut mmap = unsafe { memmap2::MmapMut::map_copy(&file)? };

    // 2. Validate 128-byte header
    let header = DumpHeader::from_bytes(&mmap[..128])?;
    header.validate_magic_and_version()?;

    // 3. Extract section slices from header bounds
    let cons_arena = &mut mmap[
        header.cons_arena_offset as usize
        ..header.cons_arena_offset as usize + header.cons_arena_len as usize
    ];
    let non_cons_arena = &mmap[
        header.non_cons_arena_offset as usize
        ..header.non_cons_arena_offset as usize + header.non_cons_arena_len as usize
    ];
    let object_table_bytes = &mmap[
        header.object_table_offset as usize..
    ];
    let object_table: &[ObjectTableEntry] = unsafe {
        std::slice::from_raw_parts(
            object_table_bytes.as_ptr() as *const ObjectTableEntry,
            header.object_table_count as usize,
        )
    };
    let metadata_bytes = &mmap[
        header.metadata_offset as usize
        ..header.metadata_offset as usize + header.metadata_len as usize
    ];

    // 4. Relocate cons arena in place (~2ms for ~80K cons cells)
    let cons_base = cons_arena.as_ptr() as usize;
    relocate_cons_arena(cons_arena, cons_base, object_table);

    // 5. Access non-cons metadata via rkyv zero-copy.
    //    The non-cons arena stores objects serialized with rkyv using
    //    the SAME type definitions from pdump/types.rs (DumpString,
    //    DumpByteCodeFunction, DumpHashTable, etc.) — all fields
    //    preserved, just a different serialization codec.
    //    ArchivedDumpMetadata mirrors DumpContextState (types.rs:1031).

    // 6. Rehydrate non-cons objects (~15ms for ~20K objects)
    //    Two-pass: allocate live objects, then resolve references.
    //    Uses actual TaggedHeap APIs:
    //      alloc_string(LispString)        — gc.rs:779
    //      alloc_float(f64)                — gc.rs:837
    //      alloc_vector(Vec<TaggedValue>)  — gc.rs:986
    //      alloc_record(Vec<TaggedValue>)  — gc.rs similar
    //      alloc_lambda(Vec<TaggedValue>)  — gc.rs:895
    //      alloc_bytecode(ByteCodeFunction)
    //      alloc_hash_table(LispHashTable)
    //      alloc_overlay(OverlayData)
    //      alloc_marker(MarkerData)
    let (heap, offset_to_live) = rehydrate_non_cons_objects(
        non_cons_arena, cons_base, object_table, /* ... */
    );

    // 7. Set up dump-cons-aware heap
    //    TaggedHeap gains dump_cons_region so GC knows cons cells in
    //    the mmap range are always-live and never swept.
    //    heap.set_dump_cons_region(cons_base, cons_arena_len, mmap);

    // 8. Build DumpContextState from archived metadata + resolved values.
    //    This is the SAME DumpContextState type the v1 loader uses
    //    (pdump/types.rs:1031). The only difference is that v2
    //    populates it from rkyv-archived data instead of bincode.
    //    Then call the existing Context::from_dump() (eval.rs:3819)
    //    which restores the full evaluator state: interner, obarray,
    //    buffer manager, autoloads, coding systems, charsets, modes,
    //    fontsets, load history, features, syntax tables, etc.
    let dump_state = build_dump_context_state(
        metadata_bytes, cons_base, &offset_to_live,
    )?;
    Context::from_dump(heap, dump_state)
}
```

### Relocation Pass (Cons Arena Only)

Only the cons arena is relocated in place. Non-cons objects are
rehydrated (copied into live Rust heap objects), so their pointer
slots are resolved during rehydration, not via in-place relocation.

The object table (persisted in the dump file) tells the loader
where each cons cell is in the cons arena. Since all cons cells
are fixed-size (16 bytes) with exactly 2 pointer slots (car + cdr),
the relocation is a simple sweep:

```rust
fn relocate_cons_arena(
    cons_arena: &mut [u8],
    cons_base_addr: usize,
    object_table: &[ObjectTableEntry],
) {
    // Walk the persisted object table — it was written at dump
    // time and stored in the Object Table section of the file.
    for entry in object_table {
        if entry.arena != ARENA_CONS { continue; }
        debug_assert_eq!(entry.type_tag, OBJ_CONS);
        let cons = unsafe {
            &mut *(cons_arena.as_mut_ptr().add(entry.offset as usize)
                   as *mut DumpConsCell)
        };
        cons.car.relocate(cons_base_addr);
        cons.cdr.relocate(cons_base_addr);
    }
}
```

### Rehydration Pass (Non-Cons Objects)

Non-cons objects cannot be used directly from the mmap because
their live layouts contain Rust heap containers (Vec, HashMap,
OnceLock). The loader accesses the rkyv-archived Dump* types
(same types as pdump/types.rs) zero-copy, then calls the EXISTING
restore functions in `pdump/convert.rs` to allocate live objects.

The rehydration reuses the same conversion functions the v1 loader
already uses (convert.rs:1672 for strings, convert.rs for vectors,
etc.), with one change: where v1 passes bincode-deserialized
`DumpString`/`DumpVector`/etc., v2 passes rkyv-archived references
to the same types. The TaggedHeap allocation APIs (gc.rs) and
mutation APIs (mutate.rs) are unchanged.

**Two-pass rehydration** to handle inter-object references:

1. Walk the object table. For each non-cons entry, access the
   archived Dump* type from the non-cons arena (rkyv zero-copy).
   Call the appropriate `TaggedHeap::alloc_*` to create a live
   object with placeholder TaggedValue::NIL slots. Record the
   mapping (arena offset -> live TaggedValue) in a HashMap.

2. Walk again. For each object with TaggedValue slots (vectors,
   lambdas, bytecode constants, hash table keys/values, overlay
   plists), resolve each archived DumpTaggedValue to a live
   TaggedValue using `resolve_dump_value()`, then write into the
   live object via `mutate.rs` helpers (`set_vector_slot` at
   mutate.rs:61, `with_closure_slots_mut` at mutate.rs:110, etc.).

```rust
/// Resolve a DumpTaggedValue to a live TaggedValue.
fn resolve_dump_value(
    dv: &DumpTaggedValue,
    cons_base_addr: usize,
    offset_to_live: &HashMap<u32, TaggedValue>,
) -> TaggedValue {
    if dv.is_immediate() {
        // Fixnum, symbol, nil, t — same bit pattern
        TaggedValue(dv.0 as usize)
    } else if dv.is_cons_pointer() {
        // Cons: arena offset -> absolute pointer into mmap'd cons arena
        let offset = dv.pointer_bits() as usize;
        let abs = cons_base_addr + offset;
        // TaggedValue::make_cons_ptr(abs) — value.rs:165
        TaggedValue::make_cons_ptr(abs as *const ConsCell)
    } else {
        // Non-cons heap: lookup in rehydrated object map
        let offset = dv.pointer_bits() as u32;
        offset_to_live.get(&offset).copied()
            .unwrap_or(TaggedValue::NIL)
    }
}
```

### Full Evaluator State Restore

The v2 loader populates a `DumpContextState` (pdump/types.rs:1031)
from the rkyv-archived metadata, then calls the existing
`Context::from_dump()` (eval.rs:3819) which restores the full
evaluator state. `DumpContextState` covers:

- Interner string table
- Obarray symbol data (value cells, function cells, plists)
- Buffer manager (all buffers, text content, markers)
- Autoload registry
- Coding system manager
- Charset registry
- Mode registry
- Fontset registry
- Load history, features list, require stack
- Syntax table cache, category table
- Process manager state, keyboard macro state
- Pre-command/post-command hooks, special variable bindings
- (full list at pdump/types.rs:1031, ~40 fields)

The `ArchivedDumpMetadata` struct is derived from `DumpContextState`
via `#[derive(Archive, Serialize)]`. Every field that contains a
`DumpValue` (the v1 dump's TaggedValue equivalent) uses
`DumpTaggedValue` in v2 and is resolved via `offset_to_live`.

The v2 change is only in HOW `DumpContextState` is populated:
rkyv zero-copy access + resolved heap pointers, instead of
bincode deserialization. The `Context::from_dump()` call and
everything downstream is unchanged.

### DumpTaggedValue Relocation

```rust
impl DumpTaggedValue {
    #[inline]
    fn relocate(&mut self, cons_base_addr: usize) {
        if self.is_cons_pointer() {
            // Cons pointer: offset -> absolute pointer into mmap'd cons arena
            let offset = self.pointer_bits() as usize;
            let abs = cons_base_addr + offset;
            self.0 = (abs as u64) | (self.0 & TAG_MASK);
        }
        // Non-cons heap pointers (string, float, veclike) are resolved
        // during rehydration via offset_to_live map, not in-place.
        // Immediates (fixnum, symbol, nil, t) are unchanged.
    }
}
```
```

---

## GC Integration

### What Is Dump-Resident

After loading, only **cons cells** are dump-resident (living in the
mmap'd cons arena). All other heap objects (strings, floats, vectors,
hash tables, bytecode, lambdas, etc.) were rehydrated into normal
Rust heap allocations during the load phase. They are normal
heap-allocated objects subject to normal GC.

This simplifies GC integration enormously: the only dump-resident
type is ConsCell, and cons cells already use a separate allocation
strategy (ConsBlock allocator with external mark bitmap).

### Dump-Resident Cons Detection

```rust
impl TaggedHeap {
    dump_cons_region: Option<Box<DumpConsRegion>>,

    #[inline]
    pub fn is_dump_cons(&self, ptr: *const ConsCell) -> bool {
        if let Some(ref dump) = self.dump_cons_region {
            let addr = ptr as usize;
            let base = dump.base as usize;
            addr >= base && addr < base + dump.len
        } else {
            false
        }
    }
}
```

### Mark Phase

- Dump-resident cons cells are implicitly "always live" (never collected)
- Dump cons children: car/cdr can be:
  - Other dump cons cells (also always live)
  - Immediates (fixnum, symbol, nil — no tracing needed)
  - Rehydrated heap objects (string, vector, etc. — must be traced)
- Therefore: the GC MUST trace through dump-resident cons cells
  to find references to rehydrated heap objects
- Optimization: only trace dump cons cells that are reachable from
  roots. Since dump cons are not swept, unreachable dump conses
  just waste mmap pages (the OS reclaims physical memory via
  demand paging)

### Sweep Phase

- Never free dump-resident cons cells (they live in the mmap region,
  outside the ConsBlock allocator)
- All other objects are normal heap allocations — sweep as today

### Conservative Stack Scan

```rust
fn is_valid_heap_pointer(&self, val: TaggedValue) -> bool {
    // Check dump cons region
    if val.tag() == TAG_CONS {
        if let Some(ref dump) = self.dump_cons_region {
            if dump.contains_ptr(val.as_cons_ptr()) { return true; }
        }
    }
    // Then check normal heap
    self.owns_non_cons_object(val.as_ptr()) || self.owns_cons_ptr(val.as_ptr())
}
```

### Mutation of Dump-Resident Cons Cells

Dump cons cells are mmap'd with MAP_PRIVATE (copy-on-write). When
Lisp code calls setcar/setcdr on a dump cons, the OS transparently
copies the 4KB page and the mutation succeeds. No special handling
is needed at the mutation site.

The GC already traces through dump cons cells (they're reachable
from roots), so mutations that store new heap pointers into dump
cons cells are naturally discovered during the mark phase.

### Mutation of Rehydrated Objects

All non-cons heap objects were rehydrated into normal Rust heap
allocations. Mutations go through the centralized helpers in
`mutate.rs`. These helpers already call `note_heap_slot_write` /
`note_heap_write` for GC write tracking. No changes needed —
rehydrated objects are normal heap objects from the GC's perspective.

The full list of mutation paths in `mutate.rs` (17 functions) that
are already covered by existing write tracking:

1. `set_cons_car` — cons car mutation
2. `set_cons_cdr` — cons cdr mutation
3. `with_vector_data_mut` — vector slot mutation
4. `replace_vector_data` — vector replacement
5. `set_vector_slot` — single vector slot
6. `with_record_data_mut` — record mutation
7. `replace_record_data` — record replacement
8. `set_record_slot` — single record slot
9. `with_closure_slots_mut` — lambda/macro mutation
10. `replace_closure_slots` — closure replacement
11. `set_closure_slot` — single closure slot
12. `with_string_text_props_mut` — string text props
13. `with_lisp_string_mut` — string data
14. `with_hash_table_mut` — hash table mutation
15. `with_bytecode_data_mut` — bytecode mutation
16. `with_marker_data_mut` — marker mutation
17. `with_overlay_data_mut` — overlay mutation

Since rehydrated objects are normal heap objects, ALL of these work
unchanged. Only cons mutations (#1, #2) can target dump-resident
objects, and those are handled by MAP_PRIVATE COW + GC tracing.

---

## Symbol Interning Across Dump Boundary

SymId is a u32 index into the global StringInterner. The dump
serializes the interner's string table in order. On load, strings
are re-interned at the same indices:

```rust
fn rebuild_interner(archived_strings: &ArchivedVec<ArchivedString>) {
    for (i, s) in archived_strings.iter().enumerate() {
        // archived string points into the mmap — zero copy
        let name: &str = s.as_str();
        interner_push_at_index(name, SymId(i as u32));
    }
}
```

New symbols interned after load get SymIds beyond the dump range.
The interner is append-only, so dump SymIds remain stable.

**Optimization**: interner strings can stay as &str slices into the
mmap region (avoiding String heap allocation for ~5K symbol names,
saving ~500KB).

---

## Performance Projection

| Phase | Current (bincode) | Projected (rkyv) |
|-------|-------------------|------------------|
| File I/O | 5ms (read 13MB) | <0.1ms (mmap) |
| Checksum | 10ms | 0ms (skip or lazy) |
| Cons deserialize | ~50ms (80K allocs) | 2ms (relocate, zero-copy) |
| Non-cons deserialize | ~30ms (20K allocs) | 15ms (rehydrate ~20K objs) |
| Interner + obarray | 10ms | 2ms |
| Runtime hookup | 5ms | 1ms |
| **Total** | **~150ms** | **~20ms** |

The ~20ms projection is conservative. The big win is cons cells
(~80% of objects by count): zero-copy via mmap + relocation (~2ms).
Non-cons objects must be rehydrated but from a compact inline format
(memcpy + Vec::from, no parsing), which is 2-3x faster than full
serde deserialization.

**Comparison to GNU Emacs (~30ms):** NeoMacs should match or beat
GNU because (a) cons cells are smaller (16 bytes vs GNU's 16 + tag),
(b) the dump is smaller (13MB vs 40MB), (c) only cons arena needs
relocation (non-cons objects are copied, not relocated).

Memory: 13MB mmap (cons arena, shared, demand-paged) + ~15MB
rehydrated heap objects = ~28MB peak, down from ~76MB peak with
bincode.

---

## Tasks

### Task 1: Add dependencies and define core types

**Files:**
- Modify: `neovm-core/Cargo.toml`
- Create: `neovm-core/src/emacs_core/pdump/arena.rs`

- [ ] **Step 1:** Add `memmap2 = "0.9"`, `rkyv = "0.8"`, `bytemuck = "1"` to Cargo.toml
- [ ] **Step 2:** Define `DumpTaggedValue` with tag encoding matching TaggedValue
- [ ] **Step 3:** Define `ArchivedConsCell`, `ArchivedLispString`, `ArchivedFloat`, `ArchivedVecLike`, `ArchivedByteCode`, `ArchivedHashTable` as `#[repr(C)]` structs
- [ ] **Step 4:** Define `DumpHeader` with magic, version, section offsets
- [ ] **Step 5:** Write unit tests: DumpTaggedValue round-trip for each tag type
- [ ] **Step 6:** Commit

### Task 2: Arena builder (dump side)

**Files:**
- Create: `neovm-core/src/emacs_core/pdump/arena_builder.rs`

- [ ] **Step 1:** Implement `ArenaBuilder` struct with `buf: Vec<u8>`, `ptr_to_offset: HashMap<usize, u64>`, `object_table: Vec<ObjectEntry>`
- [ ] **Step 2:** Implement `dump_cons()` — depth-first, dedup via ptr_to_offset, write ArchivedConsCell at aligned offset
- [ ] **Step 3:** Implement `dump_string()` — write header + inline bytes + alignment padding
- [ ] **Step 4:** Implement `dump_float()`, `dump_veclike()`, `dump_bytecode()`, `dump_hashtable()`
- [ ] **Step 5:** Implement `dump_value()` — dispatch on tag, return DumpTaggedValue
- [ ] **Step 6:** Write tests: build arena from hand-constructed Lisp heap, verify byte layout
- [ ] **Step 7:** Commit

### Task 3: rkyv metadata serialization

**Files:**
- Create: `neovm-core/src/emacs_core/pdump/metadata.rs`

- [ ] **Step 1:** Define `DumpMetadata` with `#[derive(Archive, Serialize, Deserialize)]` mirroring fields from `DumpContextState` (interner, obarray symbols, features, buffer manager, autoloads, etc.)
- [ ] **Step 2:** Implement conversion from live Context to DumpMetadata
- [ ] **Step 3:** Write tests: serialize + access via rkyv, verify field values
- [ ] **Step 4:** Commit

### Task 4: Dump file assembly (dump_to_file_v2)

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/mod.rs`

- [ ] **Step 1:** Implement `dump_to_file_v2()` that walks roots and builds: (a) cons arena via ArenaBuilder, (b) non-cons arena via ArenaBuilder, (c) object table from both arenas, (d) rkyv-serialized metadata mirroring all fields of DumpContextState, (e) root table
- [ ] **Step 2:** Write file in section order: header (128 bytes) + cons arena + non-cons arena + object table + metadata + roots. Fill header with correct offsets/lengths for all 6 sections.
- [ ] **Step 3:** Write test: dump a bootstrap Context, verify file header has correct section bounds, verify cons arena contains expected cons cells, verify object table entry count matches total objects
- [ ] **Step 4:** Commit

### Task 5: mmap loader with cons relocation + non-cons rehydration

**Files:**
- Create: `neovm-core/src/emacs_core/pdump/loader_v2.rs`

- [ ] **Step 1:** Implement `load_dump_v2()` — mmap file with MAP_PRIVATE, validate 128-byte header, extract section slices using header offsets
- [ ] **Step 2:** Implement `relocate_cons_arena()` — walk object table entries where arena==CONS, rewrite each cons cell's car/cdr DumpTaggedValues from offsets to absolute pointers using cons arena base address
- [ ] **Step 3:** Implement `rehydrate_non_cons_objects()` — two-pass: (pass 1) allocate live objects via actual TaggedHeap APIs (`alloc_string(LispString)`, `alloc_float(f64)`, `alloc_vector(Vec<TaggedValue>)`, etc.); (pass 2) resolve DumpTaggedValue slots to live TaggedValues and write into allocated objects
- [ ] **Step 4:** Implement `rebuild_interner()` from archived strings
- [ ] **Step 5:** Populate a `DumpContextState` from the rkyv-archived metadata + resolved heap objects, then call the existing `Context::from_dump()` (eval.rs:3819) to restore full evaluator state. This reuses the current restore surface unchanged.
- [ ] **Step 6:** Write test: dump + load round-trip, verify eval of `(+ 1 2)`, `(car '(a b))`, `(symbol-value 'load-path)`
- [ ] **Step 7:** Commit

### Task 6: DumpConsRegion integration into TaggedHeap

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs`
- Modify: `neovm-core/src/tagged/value.rs`

- [ ] **Step 1:** Add `dump_cons_region: Option<Box<DumpConsRegion>>` to TaggedHeap (NOT a generic "dump region" — only cons cells are dump-resident)
- [ ] **Step 2:** Implement `is_dump_cons(ptr) -> bool` pointer-range check (O(1) comparison)
- [ ] **Step 3:** Implement `new_with_dump_cons()` constructor that takes the mmap'd cons arena ownership
- [ ] **Step 4:** GC mark phase: when tracing a cons cell, check `is_dump_cons()`. If yes, do NOT mark it (dump conses are always live). But DO trace its car and cdr — they may reference rehydrated heap objects that need marking.
- [ ] **Step 5:** GC sweep phase: skip dump-resident cons pointers in the ConsBlock sweep (they're outside ConsBlock allocator memory)
- [ ] **Step 6:** Conservative stack scan: accept dump-cons-region pointers as valid in `is_valid_heap_pointer()`
- [ ] **Step 7:** Write tests: (a) GC with mix of dump cons and heap cons, verify heap cons is collected but dump cons survives; (b) dump cons pointing to heap string, verify string is traced and not collected; (c) heap cons pointing to dump cons, verify dump cons is traced but not freed
- [ ] **Step 8:** Commit

### Task 7: GC tracing through dump-resident cons cells

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs`

- [ ] **Step 1:** GC mark phase: when tracing a cons cell, check if it's dump-resident via `is_dump_cons()`. If yes, still trace car/cdr (they may point to rehydrated heap objects). Do NOT mark it (dump conses are always live, not swept).
- [ ] **Step 2:** GC sweep phase: skip dump-resident cons pointers (they're outside the ConsBlock allocator). Add an `is_dump_cons` check in the sweep loop.
- [ ] **Step 3:** MAP_PRIVATE mmap makes dump pages copy-on-write at OS level — setcar/setcdr mutations "just work" at the memory level. No explicit write barrier needed for cons cells because the GC already traces through all reachable conses.
- [ ] **Step 4:** Non-cons objects are all rehydrated into normal heap allocations. All 17 mutation paths in `mutate.rs` already have write tracking via `note_heap_slot_write` / `note_heap_write`. No changes needed.
- [ ] **Step 5:** Write tests: create dump with cons pointing to string, rehydrate string to heap, mutate dump cons car to point to NEW heap string, verify GC traces correctly and collects the old string
- [ ] **Step 6:** Write tests: verify dump cons that is unreachable from roots does NOT prevent GC of rehydrated objects it points to (dump cons itself stays in mmap forever, but objects it points to can be collected if no live reference exists)
- [ ] **Step 7:** Commit

### Task 8: Bootstrap integration and switchover

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/mod.rs`
- Modify: `neovm-core/src/emacs_core/load.rs`
- Modify: `xtask/src/main.rs`

- [ ] **Step 1:** Wire `dump_to_file_v2()` into the xtask bootstrap pipeline (neomacs-temacs --temacs=pdump)
- [ ] **Step 2:** Wire `load_dump_v2()` into `load_runtime_image()` with FORMAT_VERSION check (fall back to v1 bincode for old dumps)
- [ ] **Step 3:** Run full bootstrap: temacs -> bootstrap pdump -> byte-compile -> final pdump (all v2 format)
- [ ] **Step 4:** Run `neovm_loadup_bootstrap` test with v2 dumps
- [ ] **Step 5:** Benchmark: measure startup time (batch mode, release build)
- [ ] **Step 6:** Commit

### Task 9: Optimization pass

**Files:**
- Various

- [ ] **Step 1:** Profile: identify remaining hot spots in load_dump_v2
- [ ] **Step 2:** Lazy interner: keep symbol names as &str into mmap region (avoid String alloc for ~5K names)
- [ ] **Step 3:** Arena section ordering: place frequently-accessed objects (obarray symbols, loaded feature conses) at the start for page-fault friendliness
- [ ] **Step 4:** Optional: skip SHA-256 checksum (trust the file, add --verify flag for debug)
- [ ] **Step 5:** Benchmark again, compare to GNU Emacs
- [ ] **Step 6:** Commit

---

## Risks

1. **Alignment**: Cons arena must pad cells to 16-byte alignment (natural for ConsCell). Non-cons arena alignment is irrelevant since objects are rehydrated, not accessed in-place.

2. **Versioning**: Any change to dump struct layouts invalidates old dumps. Mitigated by FORMAT_VERSION in header; dumps are regenerated from bootstrap. Same as current bincode approach.

3. **Mutation of dump cons cells**: MAP_PRIVATE gives transparent COW at page granularity. A single setcar mutates 16 bytes but dirties a 4KB page (~256 cons cells). The GC traces all reachable cons cells anyway, so dirty pages don't need special tracking.

4. **Release pdump SIGSEGV**: The current release-mode pdump generation crashes (SIGSEGV at dump time). This is a pre-existing bug in the current dump code that must be fixed before or during this migration.

5. **Conservative stack scan**: The `is_valid_heap_pointer` check must accept dump-cons-region pointers. Without this, the conservative GC would miss references to dump-resident cons cells on the stack.

6. **mmap lifetime**: The cons arena Mmap must live as long as any dump-resident cons pointer is accessible (= process lifetime). It must be stored in TaggedHeap and never dropped.

7. **Endianness**: The arena uses native byte order (little-endian on x86-64). Cross-architecture dump sharing is not supported (same as GNU Emacs).

8. **Dump file size**: The cons arena with alignment padding may be 10-15% larger than bincode for cons data. Non-cons inline format is similar in size to bincode. Total file may be slightly larger but is mmap'd (only accessed pages faulted in).

9. **Rehydration ordering**: Non-cons objects may reference other non-cons objects (e.g., a vector slot pointing to a string). The rehydration pass must process objects in dependency order, or use a two-pass approach (allocate all objects first with placeholder pointers, then fixup). The object table's ordering at dump time can ensure dependencies are written before dependents.

10. **Live layout divergence**: The dump inline format (DumpString, DumpVector, etc.) is separate from the live layout (StringObj, VectorObj, etc.). Any change to a live struct's fields requires updating both the live type AND the dump type. Unit tests that round-trip dump+load catch this.

## Review Corrections (Rev 3)

Fixes three issues from third review:

1. **Stale loader pseudocode.** Fixed: Load Process section now uses
   128-byte header, cons_arena/non_cons_arena section slices,
   `Context::from_dump()` (eval.rs:3819, the existing restore entry
   point), and actual API names (`TaggedValue::make_cons_ptr` at
   value.rs:165, `set_vector_slot` at mutate.rs:61, etc.).

2. **Lossy inline formats.** Fixed: non-cons arena no longer defines
   new reduced types. Instead, it uses the SAME Dump* types from
   pdump/types.rs (DumpString with text_props, DumpByteCodeFunction
   with parameter names + gnu_byte_offset_map, DumpHashTable with
   key_snapshots + insertion_order, etc.) serialized via rkyv. The
   existing convert.rs restore functions are reused unchanged.

3. **Overlapping type_tag enum.** Fixed: replaced "HeapObjectKind or
   VecLikeType" with a unified `DumpObjectKind` enum with
   non-overlapping discriminants (Cons=0, String=1, Float=2,
   Vector=3, ..., Bignum=12). The cons/non-cons arena distinction
   is implicit from the kind value, not a separate `arena` field.

## Review Corrections (Rev 2)

Fixes three issues from second review:

1. **Header size and missing arena bounds.** Fixed: header expanded
   to 128 bytes with explicit fields for cons_arena_offset/len,
   non_cons_arena_offset/len, object_table_offset/count,
   metadata_offset/len, roots_offset/count, and checksum. All
   sections are unambiguously locatable.

2. **Tasks reflected old single-arena design.** Fixed: Task 4 now
   writes all 6 sections (header + cons arena + non-cons arena +
   object table + metadata + roots). Task 5 now has separate
   relocate_cons_arena and rehydrate_non_cons_objects steps. Task 6
   now says "trace through dump-resident conses" (not "skip").

3. **Rehydration used nonexistent APIs.** Fixed: pseudocode now uses
   actual TaggedHeap APIs (alloc_string(LispString), alloc_vector
   (Vec<TaggedValue>), etc.). Full evaluator restore reuses the
   existing DumpContextState + Context::from_dump() path unchanged.

## Review Corrections (Rev 1)

This plan was revised after review feedback identifying three high-severity issues:

1. **Archived layouts did not match live layouts.** Fixed: the plan now uses a two-arena design. Cons cells (no GcHeader, external bitmap) ARE mmap'd directly with matching layout. All other types use a separate dump-specific inline format and are rehydrated into live Rust heap objects on load. The archived DumpString/DumpVector/etc. are explicitly NOT the live StringObj/VectorObj layouts.

2. **Object table was never persisted.** Fixed: the file layout now includes an explicit Object Table section between the arenas and metadata. Each entry is 8 bytes (arena, type_tag, offset, slot_count). The relocation pass and rehydration pass both read this persisted table.

3. **Write barrier only covered cons cells.** Fixed: the plan now explains that only cons cells are dump-resident (all other types are rehydrated into normal heap allocations). The 17 mutation paths in mutate.rs already have write tracking via note_heap_slot_write and do not need changes. Only cons mutations can target dump-resident objects, and the GC traces through all reachable conses naturally.
