# Marker GNU-Parity Refactor Design

## Goal

Replace NeoMacs's two-layer marker architecture with a single-layer design that matches GNU Emacs's `struct Lisp_Marker`. The `TaggedHeap.marker_ptrs: Vec<*mut MarkerObj>` global bookkeeping Vec goes away, along with the parallel `BufferTextStorage.markers: Vec<MarkerEntry>`.

## Motivation

NeoMacs today splits marker state across two structures:

- **`MarkerObj`** (heap Value): carries `buffer`, a *stale-cached* `position`, `insertion_type`, `marker_id`.
- **`BufferTextStorage.markers: Vec<MarkerEntry>`** (per-buffer): holds the authoritative `byte_pos` / `char_pos`, keyed by `marker_id`.

The two layers are kept in sync manually through `marker_id` lookups. A third Vec, `TaggedHeap.marker_ptrs`, exists only so GC and `kill-buffer` can find "all markers" for cross-buffer cleanup.

This is three parallel structures for data GNU Emacs keeps in one place. Symptoms:

- `clear_markers_for_buffers` (gc.rs:980) reads every `MarkerObj` through `marker_ptrs` to null out killed buffers' refs, instead of walking just the dead buffer's own chain.
- The `marker_ptrs.retain(|ptr| ...marked)` prune in `sweep_objects` is a constant source of UAF bugs (one was just fixed in commit bd6c28266).
- `MarkerData.position` is a stale cache — readers have to know to resolve via `marker_id` → `BufferText.markers[i]` instead of trusting it.

GNU's design collapses all this: `struct Lisp_Marker` carries its own position, its own buffer pointer, and an intrusive `next_marker` link. The buffer owns `buffer->own_text.markers` (chain head). There is no global marker list. Unchain happens per-buffer (`sweep_buffer` → `unchain_dead_markers`) before the generic alloc-block sweep runs.

Matching this eliminates all three layers' failure modes at once and aligns with the user-stated goal of keeping NeoMacs architecturally close to GNU.

## Architecture

### New `MarkerData` (replaces both current `MarkerData` and `MarkerEntry`)

```rust
pub struct MarkerData {
    pub buffer: Option<BufferId>,       // None when detached
    pub bytepos: usize,                 // byte offset in buffer (authoritative)
    pub charpos: usize,                 // char offset in buffer (authoritative)
    pub insertion_type: bool,           // GNU: before/after
    pub next_marker: *mut MarkerObj,    // intrusive chain link
    pub marker_id: u64,                 // stable ID; used by pdump round-trip only
}
```

- Runtime operations use pointer identity (the `*mut MarkerObj` itself) or the `MarkerObj` Value.
- `marker_id` stays but is demoted to a pdump-stability identifier. The `MARK_MARKER_ID` sentinel usage migrates to pointer-equality against the buffer's mark marker slot.

### Buffer side

`BufferTextStorage` gains a `markers_head: *mut MarkerObj` field (chain head for this buffer).

`BufferTextStorage.markers: Vec<MarkerEntry>` is deleted.

`MarkerEntry` struct is deleted.

### Operations (match GNU `marker.c` semantics)

All chain manipulation is singly-linked, matching GNU:

- **`make-marker`**: allocate `MarkerObj { buffer: None, bytepos: 0, charpos: 0, next_marker: null, insertion_type: false, marker_id: fresh }`. Not on any chain.
- **`set-marker` / `set_marker_both`**: if old buffer is set, unlink from its chain. If new buffer is set, splice at head of new buffer's `markers_head` chain and update bytepos/charpos. If new buffer is None, leave detached.
- **`copy-marker` / `point-marker`**: allocate + splice in one step.
- **`unchain_marker(m)`**: walk `m.buffer.markers_head` chain with a `prev` tracker; when found, `*prev = m.next_marker; m.buffer = None; m.next_marker = null`. Mirrors GNU `marker.c:635`.
- **`adjust_markers_for_insert(buf, from_byte, to_byte, from_char, to_char)`**: walk `buf.markers_head`; for each `m`, apply GNU rules from `marker.c:268` (insertion_type governs whether an exact-match marker moves or stays).
- **`adjust_markers_for_delete(buf, from_char, to_char, from_byte, to_byte)`**: walk `buf.markers_head`; collapse markers inside the deleted range per GNU `marker.c:364`.
- **`kill-buffer(B)`**: walk `B.markers_head`, set each marker's `buffer = None`, clear bytepos/charpos, set `B.markers_head = null`. Replaces today's two-step (`clear_markers_for_buffers` + `clear_markers`).

### GC sweep integration

`complete_collection` today (gc.rs:1085) runs `mark_all → sweep_cons → sweep_objects`. The current `sweep_objects` has a `marker_ptrs.retain` prune step (commit bd6c28266 fix). That goes away.

Replacement: a new `unchain_dead_markers()` pass runs between `mark_all` and `sweep_objects`. For every live buffer in `TaggedHeap.buffer_registry`, walk the buffer's `markers_head` chain with a `prev` tracker:

- If the current marker is marked → keep it, advance `prev`.
- If unmarked → splice out of chain; leave the marker's fields stale (the object is about to be freed in `sweep_objects`).

After this pass, every `markers_head` chain contains only live markers. The generic `sweep_objects` walk then frees the unlinked dead markers the same way it frees any other unmarked veclike.

This mirrors GNU's `gc_sweep` → `sweep_buffer` → `unchain_dead_markers` order.

### TaggedHeap cleanup

Delete:
- `TaggedHeap.marker_ptrs: Vec<*mut MarkerObj>` field
- `alloc_marker`'s `self.marker_ptrs.push(raw)`
- `clear_markers_for_buffers(&HashSet<BufferId>)` method
- The `marker_ptrs.retain` block in `sweep_objects`
- The per-buffer `clear_markers` / `remove_markers_for_buffers` methods on `BufferText` (subsumed by the chain walk in kill-buffer)

### Pdump format

Bump format version (current v25 → v26).

Changes to `pdump/types.rs`:
- Delete `DumpMarkerEntry` struct.
- Change `DumpBuffer.markers: Vec<DumpMarkerEntry>` into `DumpBuffer.markers: Vec<DumpMarker>` holding the chain in walk order (head → tail).
- `DumpMarker` gains `bytepos`, `charpos` directly (dropped `position` cache and the separate entry side-table).

On load, iterate each `DumpBuffer.markers` list and for each marker allocate the `MarkerObj`, set buffer/bytepos/charpos, splice at chain tail. Keep insertion order so chain is deterministic.

The user has explicitly said prior pdump incompatibility is fine — old dumps are discarded and regenerated.

### Identity: marker_id vs pointer

GNU uses pointer identity. NeoMacs kept `marker_id` so buffer-text-level code can refer to a marker without holding the heap pointer.

Post-refactor, runtime operations use pointer identity (matching GNU). `marker_id` is retained only to:
1. Support pdump round-trip identity (so that a marker's identity is stable across save/load).
2. Give `MARK_MARKER_ID` a stable handle for the special "(point-)mark" marker (alternative: a dedicated field on `BufferTextStorage`). TBD during planning — if we can keep `MARK_MARKER_ID` as a sentinel buffer-field instead of a free-standing ID, marker_id can potentially be dropped entirely. Default is "keep marker_id for now; drop in a follow-up if feasible."

## Components touched

- `neovm-core/src/heap_types.rs` — `MarkerData` struct definition.
- `neovm-core/src/tagged/gc.rs` — delete `marker_ptrs`, `clear_markers_for_buffers`, `sweep_objects` prune; add `unchain_dead_markers()`; `alloc_marker` loses push.
- `neovm-core/src/tagged/header.rs` — no change to MarkerObj wrapping; only `data` inner struct changes via heap_types.rs.
- `neovm-core/src/buffer/buffer_text.rs` — delete `markers: Vec<MarkerEntry>`, `MarkerEntry`, `register_marker`, `adjust_markers_for_insert/_delete`, `advance_markers_at`, `marker_entry`, `clear_markers`, `remove_markers_for_buffers`. Add `markers_head: *mut MarkerObj` and chain-walking replacements.
- `neovm-core/src/buffer/buffer.rs` — `kill_buffer_collect` simplifies; `MarkerEntry` uses removed; `register_marker_id` removed.
- `neovm-core/src/buffer/insdel.rs` — `insert_text` and `delete_text` adjust-markers callers now call the chain-walking versions (same function names, new implementations).
- `neovm-core/src/emacs_core/marker.rs` — `make_marker_value`, `make_registered_buffer_marker`, `register_marker_in_buffers`, `builtin_copy_marker`, `builtin_set_marker`, `builtin_point_marker`, `builtin_mark_marker`, `marker_position_as_int_with_buffers`, `builtin_marker_position_in_buffers`, `builtin_marker_buffer`. Readers switch to reading MarkerData directly (no `marker_entry` lookups).
- `neovm-core/src/emacs_core/pdump/types.rs` — remove `DumpMarkerEntry`, update `DumpMarker`, update `DumpBuffer`.
- `neovm-core/src/emacs_core/pdump/convert.rs` — update serialize/deserialize paths; bump format version.

## Testing

Run the existing marker test suites — all should continue to pass unchanged:

- `neovm-core/src/emacs_core/marker_test.rs`
- `neovm-core/tests/compat_marker_semantics.rs`
- `neovm-oracle-tests/src/marker-operations.rs`
- `neovm-oracle-tests/src/marker-comprehensive-patterns.rs`
- `test/src/marker-tests.el`
- Bootstrap tests (`cached_bootstrap_reload_evaluates_full_advice_remove_member_form` et al.)

Add new tests for GC sweep correctness:
- A marker with no Lisp-visible reference (unmarked during GC) must be unlinked from its buffer's chain before `sweep_objects` frees it. Verify the chain stays consistent after a forced GC.
- Buffer kill with live markers: chain is cleared, markers have `buffer: None`.

## Risks

1. **Raw pointer chain in GC-managed memory.** A marker's `next_marker` must be kept consistent across every op. Unsafe discipline required; every chain walk uses a `prev: *mut *mut MarkerObj` pattern. Mitigated by small call-site count (mapped in investigation).
2. **Sweep ordering.** `unchain_dead_markers` MUST run between `mark_all` and `sweep_objects`; if reversed, UAF. Document clearly in `complete_collection`.
3. **Pdump migration.** Format bump is cheap; existing dumps are discarded. Need tests to confirm marker chain round-trips.
4. **`MARK_MARKER_ID` sentinel.** Confirm during planning whether replacing with a direct buffer slot reference is trivial or needs to stay an ID-based lookup.

## Out of scope

- Making `marker_id` truly optional (could be a follow-up).
- Changing `Lisp_Marker` → `MarkerObj` Value tagging.
- Replacing the `BufferText` storage model more broadly.

## Success criteria

- `TaggedHeap.marker_ptrs` field does not exist.
- `MarkerEntry` struct does not exist.
- `BufferTextStorage` has a single marker field (`markers_head: *mut MarkerObj`).
- All existing marker tests pass.
- All bootstrap tests pass.
- ASAN build of `cached_bootstrap_reload_evaluates_full_advice_remove_member_form` is clean.
