# Static Subr Registry Design Spec

## Goal

Replace heap-allocated `SubrObj` with a global static subr table indexed by `SymId`, matching GNU Emacs's static `Lisp_Subr` design. This fixes pdump stale subr objects causing void-function errors for `apply`/`run-hook-wrapped` in the TUI.

## Root Cause

After pdump load, bytecoded `.elc` function constants hold references to OLD heap-allocated `SubrObj` objects that have `function = None` (pdump can't preserve Rust fn pointers) or stale `dispatch_kind`. `init_builtins` creates/updates subr objects, but bytecode constants still point to the old ones. The named call cache gets poisoned with `Void` entries, causing permanent void-function errors.

GNU Emacs doesn't have this problem because subrs are static C structs in the binary — they never move, never become stale, never need re-initialization.

## Design

### Conceptual Model

Every subr is identified by its symbol's `SymId`. Subr metadata (Rust function pointer, arity, dispatch kind) lives in a global static table keyed by `SymId`. Subrs are NOT heap objects. The tagged value for a subr encodes the `SymId` directly as an immediate value.

### Tagged Value Encoding

Subrs use a new immediate sub-tag under the `111` (Immediate) tag:

```
Bits:  [63 .................. 8][7 .. 3][2 1 0]
       SymId (u32)              00001   111
                                sub-tag  immediate tag

SUBR_IMMEDIATE = 0x0F  (sub-tag 1 << 3 | tag 0b111)
```

Construction: `(sym_id.0 as usize) << 8 | 0x0F`
Detection: `value & 0xFF == 0x0F`
Extraction: `SymId((value >> 8) as u32)`

Existing immediate sub-tags:
- `00000` (0x07): `Qunbound`
- `00001` (0x0F): **Subr** (new)

### Global Subr Table

```rust
struct SubrEntry {
    function: Option<SubrFn>,
    min_args: u16,
    max_args: Option<u16>,
    dispatch_kind: SubrDispatchKind,
    name_id: NameId,
}
```

Thread-local global table (matching existing thread-local tagged heap pattern):
`HashMap<SymId, SubrEntry>`

Populated once by `init_builtins`. Looked up by `SymId` extracted from the tagged value.

### New ValueKind Variant

```rust
pub enum ValueKind {
    // ... existing variants ...
    Subr(SymId),  // NEW — replaces Veclike(VecLikeType::Subr)
}
```

### New Methods on TaggedValue

```rust
pub fn subr_from_sym_id(sym_id: SymId) -> Self;
pub fn is_subr(self) -> bool;
pub fn as_subr_sym_id(self) -> Option<SymId>;
pub fn as_subr_entry(self) -> Option<&'static SubrEntry>;
```

### What Gets Deleted

- `SubrObj` struct (heap-allocated subr object)
- `h.alloc_subr()` (tagged heap subr allocation)
- `VecLikeType::Subr` (veclike sub-type variant)
- `subr_registry` / `subr_slot_registry` on `TaggedHeap`
- `register_current_subr()` (thread-local registration)
- `subr_value()` / `subr_slot_mut()` / `subr_ref()` on Context
- `as_subr_ref()` returning `&SubrObj`
- Pdump subr serialization (`DumpSubr`, `load_subr`, `dump_subr`)
- GC tracing for subr registry

### Call Site Migration

| Current Pattern | New Pattern |
|----------------|-------------|
| `ValueKind::Veclike(VecLikeType::Subr)` | `ValueKind::Subr(sym_id)` |
| `value.as_subr_ref() -> &SubrObj` | `value.as_subr_entry() -> &SubrEntry` (global table) |
| `defsubr_with_entry` allocs heap SubrObj | Inserts `SubrEntry` in global table + sets function cell |
| `funcall_general_untraced` Subr arm | Looks up SubrEntry by SymId, dispatches fn pointer |
| Pdump serializes SubrObj | Pdump stores SymId integer (always valid) |
| GC traces subr registry | Nothing to trace (immediates, not heap) |

### Pdump

Subr values serialize as their `SymId` integer. On load, `subr_from_sym_id(sym_id)` reproduces the exact same bit pattern. No deserialization of heap objects. No function pointer restoration. No patching.

The pdump no longer needs `DumpSubr` or any subr-specific serialization logic. Subr values are self-describing immediates like fixnums.

### GC

Subr values are immediates — the GC never traces, marks, or sweeps them. The subr registry entries don't contain heap-managed values (only `NameId` and Rust fn pointers), so no GC integration needed.

### Init Flow

```
1. Program starts
2. init_builtins() populates global SubrTable (once)
3. Each defsubr: insert SubrEntry + set symbol function cell to Value::subr_from_sym_id(sym_id)
4. Pdump load: subr values in function cells / bytecode constants are SymId-encoded immediates
5. init_builtins() runs again (same as step 2) — table repopulated, same SymIds, same bit patterns
6. No stale objects. No patching. Bytecode constants always resolve correctly.
```

### Not Changed

- `condition_stack` — stays separate
- `bc_buf` / `bc_frames` — bytecode execution stack stays separate
- `specpdl` — unified stack stays as-is
- `SpecBinding::GcRoot` — stays (precise GC)
- Symbol function cells — still store `Value`, just the encoding changes for subrs
