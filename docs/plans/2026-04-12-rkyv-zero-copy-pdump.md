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
0x0000  Header (64 bytes)
        - magic: b"NEODUMP2"
        - version: u32
        - checksum: [u8; 32] (SHA-256 of remaining sections)
        - arena_offset: u64
        - arena_len: u64
        - metadata_offset: u64
        - metadata_len: u64
        - roots_offset: u64
        - roots_len: u64

0x0040  Object Arena (hot section)
        Flat byte buffer of #[repr(C)] archived Lisp objects.
        Each object is naturally aligned (8-byte for cons/float).
        Pointer slots store DumpTaggedValue (offset-based).

        ArchivedConsCell:    16 bytes (car: DumpTaggedValue, cdr: DumpTaggedValue)
        ArchivedLispString:  12-byte header + inline bytes + padding
        ArchivedFloat:       8 bytes (f64)
        ArchivedVecLike:     8-byte header + N * DumpTaggedValue slots
        ArchivedByteCode:    variable header + ops + constants
        ArchivedHashTable:   variable header + key/value pairs

0xNNNN  Metadata (cold section, rkyv-serialized)
        ArchivedDumpMetadata containing:
        - interner_strings: Vec<String>
        - obarray_symbols: Vec<ArchivedSymbolData>
        - features: Vec<u32>
        - buffer_manager state
        - autoload manager state
        - mode/coding/charset registries

0xMMMM  Root Table
        Array of DumpTaggedValues for top-level roots
        (global variables, special forms, etc.)
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

```rust
/// 16 bytes, same as live ConsCell
#[repr(C)]
pub struct ArchivedConsCell {
    pub car: DumpTaggedValue,
    pub cdr: DumpTaggedValue,
}

/// Variable-length: 12-byte header + data bytes + alignment padding
#[repr(C)]
pub struct ArchivedLispString {
    pub size: u32,        // character count
    pub size_byte: i32,   // byte count (-1 for unibyte)
    pub data_len: u32,    // bytes of string data following header
    // [u8; data_len] follows inline
}

/// 8 bytes
#[repr(C)]
pub struct ArchivedFloat {
    pub value: f64,
}

/// Variable-length header + N DumpTaggedValue slots
#[repr(C)]
pub struct ArchivedVecLike {
    pub type_tag: u8,     // VecLikeType discriminant
    pub _pad: [u8; 3],
    pub len: u32,         // number of slots
    // [DumpTaggedValue; len] follows inline
}

/// Variable-length
#[repr(C)]
pub struct ArchivedByteCode {
    pub max_stack: u16,
    pub params_required: u16,
    pub params_optional: u16,
    pub params_rest: u8,
    pub lexical: u8,
    pub ops_len: u32,
    pub constants_len: u32,
    // [u8; ops_len] follows (bytecode ops)
    // [DumpTaggedValue; constants_len] follows (constant pool)
}

/// Variable-length
#[repr(C)]
pub struct ArchivedHashTable {
    pub test: u8,         // HashTableTest discriminant (eq/eql/equal)
    pub weakness: u8,
    pub _pad: [u8; 2],
    pub count: u32,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    // [DumpTaggedValue; count * 2] follows (key, value pairs)
}
```

---

## Load Process

```rust
pub fn load_dump_v2(path: &Path) -> Result<Context, DumpError> {
    // 1. mmap the file (MAP_PRIVATE for copy-on-write mutation support)
    let file = std::fs::File::open(path)?;
    let mmap = unsafe { memmap2::MmapMut::map_copy(&file)? };

    // 2. Validate header (64 bytes)
    let header = DumpHeader::from_bytes(&mmap[..64])?;
    header.validate()?;

    // 3. Eager relocation: rewrite DumpTaggedValues in the arena
    //    from offsets to absolute pointers.
    let arena_base = mmap[header.arena_offset..].as_ptr() as usize;
    relocate_arena(&mut mmap[header.arena_offset..], arena_base);
    // Cost: ~2ms for 200K pointer slots

    // 4. Access cold metadata via rkyv zero-copy
    let metadata = rkyv::access::<ArchivedDumpMetadata, rkyv::rancor::Error>(
        &mmap[header.metadata_offset..header.metadata_end]
    )?;

    // 5. Rebuild interner from archived strings (~1ms)
    rebuild_interner(&metadata.interner_strings);

    // 6. Create heap with dump region awareness
    let heap = TaggedHeap::new_with_dump(DumpRegion {
        base: arena_base as *const u8,
        len: header.arena_len as usize,
        mmap,
    });

    // 7. Rebuild mutable state from metadata (~2ms)
    let obarray = rebuild_obarray(&metadata.obarray_symbols);
    let buffer_mgr = rebuild_buffer_manager(&metadata.buffer_manager);
    // ... other managers ...

    // 8. Assemble Context
    Context::from_dump_v2(heap, obarray, buffer_mgr, metadata)
}
```

### Relocation Pass

```rust
fn relocate_arena(arena: &mut [u8], base_addr: usize) {
    // Walk the object table (offsets of all objects in the arena).
    // For each object, find its pointer slots and rewrite them.
    let table = parse_object_table(arena);
    for entry in &table {
        match entry.type_tag {
            OBJ_CONS => {
                let cons = unsafe {
                    &mut *(arena.as_mut_ptr().add(entry.offset) as *mut ArchivedConsCell)
                };
                cons.car.relocate(base_addr);
                cons.cdr.relocate(base_addr);
            }
            OBJ_VECLIKE => {
                // Relocate each slot in the vector
                let header = unsafe {
                    &*(arena.as_ptr().add(entry.offset) as *const ArchivedVecLike)
                };
                let slots_ptr = unsafe {
                    arena.as_mut_ptr().add(entry.offset + 8) as *mut DumpTaggedValue
                };
                for i in 0..header.len as usize {
                    unsafe { (*slots_ptr.add(i)).relocate(base_addr); }
                }
            }
            // Strings and floats have no pointer slots — skip
            OBJ_STRING | OBJ_FLOAT => {}
            OBJ_BYTECODE => { /* relocate constants array */ }
            OBJ_HASHTABLE => { /* relocate key/value pairs */ }
            _ => {}
        }
    }
}

impl DumpTaggedValue {
    #[inline]
    fn relocate(&mut self, arena_base: usize) {
        if self.is_heap_pointer() {
            let offset = self.pointer_bits() as usize;
            let abs = arena_base + offset;
            self.0 = (abs as u64) | (self.0 & TAG_MASK);
        }
        // Immediates (fixnum, symbol, nil, t) unchanged
    }
}
```

---

## GC Integration

### Dump-Resident Object Detection

```rust
impl TaggedHeap {
    dump_region: Option<Box<DumpRegion>>,

    #[inline]
    pub fn is_dump_object(&self, ptr: *const u8) -> bool {
        if let Some(ref dump) = self.dump_region {
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

- Dump-resident objects are implicitly "always live" (never collected)
- If dump objects are never mutated: skip tracing their children entirely
  (all children are also dump-resident or immediate)
- If a dump cons is mutated via setcar/setcdr (MAP_PRIVATE COW):
  the dirty page must be traced. Track dirty dump objects via a
  write barrier or page-fault handler.

### Sweep Phase

- Never free dump-resident objects (they live in the mmap region)
- Only sweep heap-allocated objects as today

### Conservative Stack Scan

```rust
fn is_valid_heap_pointer(&self, val: TaggedValue) -> bool {
    // Check dump region first
    if let Some(ref dump) = self.dump_region {
        if dump.contains_ptr(val.as_ptr()) { return true; }
    }
    // Then check normal heap
    self.owns_non_cons_object(val.as_ptr()) || self.owns_cons_ptr(val.as_ptr())
}
```

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
| Deserialize | 80ms | 0ms (zero-copy) |
| Heap reconstruct | 40ms (100K allocs) | 2ms (relocate) |
| Interner + obarray | 10ms | 2ms |
| Runtime hookup | 5ms | 1ms |
| **Total** | **~150ms** | **~5ms** |

Memory: 13MB mmap (shared, demand-paged) + ~10MB mutable copies
= ~23MB peak, down from ~76MB peak with bincode.

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

- [ ] **Step 1:** Implement `dump_to_file_v2()` that: walks roots, builds arena, serializes metadata, writes header + arena + metadata + roots to file
- [ ] **Step 2:** Write test: dump a bootstrap Context, verify file header, verify arena contains expected objects
- [ ] **Step 3:** Commit

### Task 5: mmap loader with eager relocation

**Files:**
- Create: `neovm-core/src/emacs_core/pdump/loader_v2.rs`

- [ ] **Step 1:** Implement `load_dump_v2()` — mmap file, validate header
- [ ] **Step 2:** Implement `relocate_arena()` — walk object table, rewrite DumpTaggedValues from offsets to absolute pointers
- [ ] **Step 3:** Implement `rebuild_interner()` from archived strings
- [ ] **Step 4:** Implement `rebuild_obarray()` from archived symbol data
- [ ] **Step 5:** Implement `Context::from_dump_v2()` — assemble a live Context from the relocated arena + rebuilt metadata
- [ ] **Step 6:** Write test: dump + load round-trip, verify eval of simple expressions
- [ ] **Step 7:** Commit

### Task 6: DumpRegion integration into TaggedHeap

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs`
- Modify: `neovm-core/src/tagged/value.rs`

- [ ] **Step 1:** Add `dump_region: Option<Box<DumpRegion>>` to TaggedHeap
- [ ] **Step 2:** Implement `is_dump_object()` pointer-range check
- [ ] **Step 3:** Implement `new_with_dump()` constructor
- [ ] **Step 4:** GC mark phase: skip dump-resident objects (no mark needed, always live)
- [ ] **Step 5:** GC sweep phase: never free dump-resident pointers
- [ ] **Step 6:** Conservative scan: accept dump-region pointers as valid
- [ ] **Step 7:** Write tests: GC with dump-resident + heap-allocated objects coexisting
- [ ] **Step 8:** Commit

### Task 7: Write barrier for dump mutations

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs`
- Modify: `neovm-core/src/emacs_core/builtins/cons_list.rs` (setcar/setcdr)

- [ ] **Step 1:** MAP_PRIVATE mmap makes dump pages copy-on-write at OS level — mutations "just work" at the memory level
- [ ] **Step 2:** Add dirty-dump-object tracking: when setcar/setcdr targets a dump-resident cons, record it in a dirty set
- [ ] **Step 3:** GC mark phase: trace dirty dump objects as additional roots
- [ ] **Step 4:** Write tests: mutate a dump cons, verify GC traces correctly
- [ ] **Step 5:** Commit

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

1. **Alignment**: Arena must pad objects to natural alignment (8-byte for cons/float/veclike). The arena builder must insert padding bytes. Misalignment causes UB on some architectures.

2. **Versioning**: Any change to archived struct layouts invalidates old dumps. Mitigated by FORMAT_VERSION in header; dumps are regenerated from bootstrap. Same as current bincode approach.

3. **Mutation of dump objects**: MAP_PRIVATE gives transparent COW at page granularity. A single setcar mutates 16 bytes but dirties a 4KB page. The GC must trace all objects in dirty pages. For typical startup workloads, very few dump conses are mutated.

4. **Release pdump SIGSEGV**: The current release-mode pdump generation crashes (SIGSEGV at dump time). This is a pre-existing bug in the current dump code that must be fixed before or during this migration.

5. **Conservative stack scan**: The `is_valid_heap_pointer` check must accept dump-region pointers. Without this, the conservative GC would miss references to dump-resident objects on the C stack.

6. **mmap lifetime**: The Mmap must live as long as any dump-resident pointer is accessible (= process lifetime). It must be stored in TaggedHeap and never dropped.

7. **Endianness**: The arena uses native byte order (little-endian on x86-64). Cross-architecture dump sharing is not supported (same as GNU Emacs).

8. **Dump file size**: The arena format with alignment padding may be 15-20% larger than bincode. Acceptable because the file is mmap'd (only accessed pages are faulted in).
