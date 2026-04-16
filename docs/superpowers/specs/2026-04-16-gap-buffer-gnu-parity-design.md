# Gap Buffer GNU-Parity Refactor — Design

**Date:** 2026-04-16
**Status:** Approved
**Scope:** `neovm-core/src/buffer/gap_buffer.rs` and `neovm-core/src/buffer/buffer_text.rs`

## Motivation

NeoMacs's current gap buffer works but is significantly slower than GNU Emacs's on typical editing workloads. An audit against `emacs-mirror/emacs/src/insdel.c`, `buffer.h`, and `marker.c` identified four divergences that together account for the gap. This refactor closes them.

## Audit findings

| # | Divergence | Severity | Root cause |
|---|-----------|----------|-----------|
| 1 | Gap sized 64 B default / 64 B grow (vs GNU 2000 / 20 floor) | Severe | `DEFAULT_GAP_SIZE = 64`, `MIN_GAP_GROW = 64` in `gap_buffer.rs:16-20` |
| 2 | Every `insert` / `delete` / `move_gap_to` re-scans bytes to count chars | Medium | `emacs_char_count_bytes()` calls inside the mutation path |
| 3 | `is_emacs_char_boundary` is O(n) from byte 0 | Medium (debug-path O(n²)) | Iterates `string_char` from position 0 — GNU uses a bit-check on a single byte |
| 4 | No char↔byte position cache | Severe on large buffers | `char_to_byte` / `byte_to_char` always scan a segment linearly — no anchors beyond gap start |

## Goals

- Match GNU's gap-sizing heuristic (eliminate ~30× reallocation frequency).
- Accept pre-computed `(nchars, nbytes)` on mutation hot paths; stop redundant scans.
- Make `is_char_boundary` O(1) via `CHAR_HEAD_P` bit test.
- Add a `buf_charpos_to_bytepos` / `buf_bytepos_to_charpos` path on `BufferText` that brackets queries via anchors + marker chain + last-query cache (the GNU `marker.c:167-270` algorithm).
- Preserve all current public behavior of `GapBuffer` and `BufferText`. No semantic changes visible to callers.

## Non-goals

- Renaming NeoMacs identifiers to match GNU's C names (`insert_1_both`, `GPT`, etc.). Cosmetic churn, no runtime benefit.
- `make_gap_smaller` gap shrinking. Deferred — leave gap memory in place.
- Changing `MarkerEntry` semantics or any Lisp-visible marker behavior.
- Changes to `overlay.rs`, `text_props.rs`, `undo.rs`.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│ BufferText  (Rc<RefCell<BufferTextStorage>>)        │
│                                                     │
│  ┌─────────────────────────────────────────────┐   │
│  │ pos_cache: Cell<PositionCache>              │   │← NEW
│  │   { modiff, last_charpos, last_bytepos }    │   │
│  └─────────────────────────────────────────────┘   │
│                                                     │
│  buf_charpos_to_bytepos(charpos) -> bytepos         │← NEW
│  buf_bytepos_to_charpos(bytepos) -> charpos         │← NEW
│    uses anchors: BEG, GPT, Z + markers[] +          │
│    pos_cache, scans from nearest bracket,           │
│    auto-inserts anchor markers every 5000 chars     │
│                                                     │
│  markers: Vec<MarkerEntry>   ← existing             │
│  gap: GapBuffer              ← existing, refactored │
└─────────────────────────────────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────────────────┐
│ GapBuffer                                           │
│   Gap sizing: GAP_BYTES_DFL = 2000, MIN = 20        │← UPDATED
│   is_char_boundary: O(1) CHAR_HEAD_P bit check      │← UPDATED
│                                                     │
│   insert_emacs_bytes_both(pos, bytes, nchars)       │← NEW primary
│   delete_range_both(from, to, nchars)               │← NEW primary
│   move_gap_both(bytepos, charpos)                   │← NEW primary
│                                                     │
│   insert_emacs_bytes / delete_range / move_gap_to   │← kept as
│     wrap the _both form, compute nchars once        │   wrappers
└─────────────────────────────────────────────────────┘
```

## Design details

### 1. Gap sizing (`gap_buffer.rs`)

Replace:

```rust
const DEFAULT_GAP_SIZE: usize = 64;
const MIN_GAP_GROW: usize = 64;
```

with:

```rust
/// Default extra gap bytes to pre-allocate on any growth (GNU: GAP_BYTES_DFL).
const GAP_BYTES_DFL: usize = 2000;
/// Floor for the gap after shrinking (GNU: GAP_BYTES_MIN). Currently unused
/// because make_gap_smaller is not implemented, but kept as a named constant
/// so future shrink work has a correct default.
const GAP_BYTES_MIN: usize = 20;
```

`ensure_gap(min_size)` becomes:

```rust
pub fn ensure_gap(&mut self, min_size: usize) {
    if self.gap_size() >= min_size { return; }
    // GNU insdel.c:483-484: add GAP_BYTES_DFL beyond what the caller asked for,
    // so amortized cost of sequential inserts is O(1).
    let need = min_size - self.gap_size();
    let grow = need.saturating_add(GAP_BYTES_DFL);
    // ... existing realloc logic unchanged ...
}
```

Initial gap in `new` / `from_emacs_bytes` also switches to `GAP_BYTES_DFL`.

### 2. "Both" variants on hot paths

New primary entry points that trust the caller:

```rust
/// Insert raw Emacs bytes at logical byte position `pos`, given pre-counted
/// char length. Caller must ensure `nchars == emacs_char_count_bytes(bytes)`.
pub fn insert_emacs_bytes_both(&mut self, pos: usize, bytes: &[u8], nchars: usize);

/// Delete logical byte range [from, to), given pre-counted char length.
pub fn delete_range_both(&mut self, from: usize, to: usize, nchars: usize);

/// Move the gap to `bytepos` (with known `charpos`). Avoids scanning moved
/// bytes to recompute `gap_start_chars`.
pub fn move_gap_both(&mut self, bytepos: usize, charpos: usize);
```

Existing `insert_emacs_bytes` / `delete_range` / `move_gap_to` are reduced to thin wrappers: they compute `nchars` once (free in unibyte mode) and delegate. All three current signatures are preserved.

`insdel.rs` (the insertion/deletion driver) is the primary place we'd thread pre-computed counts through to the `_both` forms. That migration is mechanical; sites that already know the count (e.g. buffer-to-buffer copies) use `_both`; others fall through the wrappers.

### 3. `is_char_boundary` O(1)

```rust
fn is_char_boundary(&self, pos: usize) -> bool {
    if !self.multibyte || pos == 0 || pos >= self.len() {
        return true;
    }
    // Emacs internal encoding: leading bytes satisfy CHAR_HEAD_P (bit7=0,
    // or bit7=1 && bit6=1). Trailing bytes have the form 10xxxxxx.
    let b = self.byte_at(pos);
    (b & 0xC0) != 0x80
}
```

The `is_emacs_char_boundary` free function used by `emacs_byte_to_char_in_slice` gets the same treatment — one-byte test, not a walk.

### 4. Position cache on `BufferText`

Data:

```rust
#[derive(Clone, Copy, Default)]
struct PositionCache {
    /// chars_modified_tick value when this entry was stored; 0 = invalid.
    modiff: i64,
    charpos: usize,
    bytepos: usize,
}

struct BufferTextStorage {
    // ... existing fields ...
    pos_cache: Cell<PositionCache>,
}
```

`Cell` gives us interior mutability for read-path writes (conversion during an immutable query); the outer `RefCell` is borrowed immutably during conversion.

#### Algorithm: `buf_charpos_to_bytepos(target: usize) -> usize`

Mirrors GNU `marker.c:167-270`.

1. **Unibyte fast path:** if `total_chars == total_bytes`, return `target`.
2. **Init bracket:**
   ```
   best_below       = (0, 0)                    // BEG
   best_above       = (total_chars, total_bytes) // Z
   ```
   Include GPT anchor `(gap_start_chars, gap_start_bytes)`.
   Include `pos_cache` if `pos_cache.modiff == chars_modified_tick`.
3. **Consider markers:** walk `markers: Vec<MarkerEntry>`, each tightens `best_below` or `best_above`. Early-bail if
   `best_above.char - target < distance` or `target - best_below.char < distance`,
   where `distance = 50 + 50 * markers_checked` (GNU `marker.c:162`).
4. **Scan:** pick the closer bracket. If scanning forward from `best_below` in the pre-gap slice, use `emacs_char::char_to_byte_pos`; if crossing the gap, split the walk at `gap_start` / `gap_end`. Symmetric for `best_above`.
5. **Auto-anchor:** if total chars walked > 5000, push a plain `MarkerEntry` at the result position (details in "Risks" below — must be internal-only, not Lisp-visible).
6. **Update cache:** `pos_cache.set(PositionCache { modiff: chars_modified_tick, charpos: target, bytepos: result })`.

`buf_bytepos_to_charpos` is symmetric.

#### Cache invalidation

Every mutation through `insdel.rs` already bumps `chars_modified_tick` via `record_char_modification` (verified at `insdel.rs:159,226,235`). The cache is invalidated automatically by the `modiff == chars_modified_tick` check. No explicit invalidation required. Mutations do *not* need to touch `pos_cache`.

**Dependency:** any new mutation path that does not route through `record_char_modification` would silently return stale cached bytepos. If new direct-mutation APIs are added to `BufferText` in the future, they must bump `chars_modified_tick`. An `debug_assert!` in `buf_charpos_to_bytepos` that verifies `cached_bytepos == fresh_scan(cached_charpos)` under `cfg(debug_assertions)` catches this regression locally during tests.

#### Caller wiring

`BufferText::byte_to_char` / `char_to_byte` currently delegate to `gap.byte_to_char` / `gap.char_to_byte`. They move to the cached implementation instead. `GapBuffer::byte_to_char` / `char_to_byte` stay (same slow code) as a fallback — used by `gap_buffer_test.rs` and anywhere a `GapBuffer` is used standalone.

## Data flow

```
Caller (insdel.rs, editfns.rs, etc.)
    │
    ▼
BufferText::char_to_byte(pos)
    │
    ▼
buf_charpos_to_bytepos(pos)
    │  [cache hit?]
    ▼
Bracket via BEG/GPT/Z/markers/pos_cache
    │
    ▼
Scan from nearest bracket over gap_buffer segment(s)
    │
    ▼
Store into pos_cache; return
```

## Error handling

- All existing panics/asserts preserved (position out-of-range, not-on-boundary).
- Cache is best-effort: if anchors become inconsistent for any reason, the scan from a bracketing anchor still produces the correct answer (correctness does not depend on cache freshness — the `modiff` check ensures staleness == cache miss).
- `debug_assert!` (behind `cfg(debug_assertions)`) added in `pos_cache` update path to verify that the computed bytepos matches a ground-truth `emacs_char::char_to_byte_pos` result on a small slice. Production builds skip this.

## Testing

### Unit tests (added to `buffer_text_test.rs`)

1. `position_cache_correctness_multibyte` — build a 100KB buffer of mixed ASCII + UTF-8 CJK, convert 1000 random positions, verify each against a ground-truth `chars_in_multibyte(&bytes[..n])` oracle.
2. `position_cache_invalidates_on_mutation` — convert, mutate, convert again at same charpos with different bytes, verify no stale result.
3. `position_cache_reuses_same_query` — convert charpos X, then convert X again without mutation, verify `pos_cache.modiff` stayed valid (no rescan happened — measured by a scan-counter test hook gated on `cfg(test)`).
4. `auto_anchor_marker_inserted_on_long_walk` — in a 50K-char multibyte buffer with no markers, convert position 45000 (forces >5000 char walk), verify an anchor marker was pushed.

### Unit tests (added to `gap_buffer_test.rs`)

5. `gap_sizing_uses_gnu_default` — insert 1 byte into a fresh buffer, verify `gap_size() >= GAP_BYTES_DFL`.
6. `is_char_boundary_is_o1` — indirect: call boundary check at many positions in a large multibyte buffer and verify results match the previous O(n) implementation bit-for-bit.
7. `insert_emacs_bytes_both_no_rescan` — same correctness as `insert_emacs_bytes`, called directly.

### Existing tests

All current `gap_buffer_test.rs` (688 lines) and `buffer_text_test.rs` must pass unchanged. This is the primary correctness guarantee.

### Performance validation (not a committed test)

Manual microbench before/after:
- (a) 1 MB sequential ASCII insert, append at end.
- (b) 1 MB UTF-8 (CJK) insert, append at end.
- (c) 1 M random `char_to_byte` queries over a 1 MB multibyte buffer.

Success targets:
- (a), (b): ≥ 3× speedup (gap sizing + no rescan dominate).
- (c): ≥ 10× speedup (position cache dominates).

Results recorded in the PR description.

## Migration

Order of changes, each compile-cleanly and test-green on its own:

1. **Gap-sizing constants** — one-line change, all tests still pass. Dramatic win on any sequential-insert workload.
2. **`is_char_boundary` O(1)** — local to `gap_buffer.rs`. Tests pass.
3. **`_both` variants + wrap old names** — adds API, removes no API. Tests pass.
4. **Thread `nchars` through `insdel.rs`** — mechanical, preserves behavior.
5. **`PositionCache` + `buf_charpos_to_bytepos` on `BufferText`** — additive, existing `byte_to_char` / `char_to_byte` delegate to new path. Tests pass.
6. **Auto-anchor marker insertion** — last step, gated on an internal flag on `MarkerEntry` (see Risks below) so Lisp visibility is preserved.

Each step is a separable commit.

## Risks

1. **`MarkerEntry` Lisp visibility.** Implementation step 6 (auto-anchor markers) must check whether `markers: Vec<MarkerEntry>` is exposed to Lisp (e.g. to `(buffer-markers)`-style queries or point-marker tracking). If exposed, auto-anchor entries need either:
   - an `internal: bool` flag on `MarkerEntry` that filters them out of Lisp-visible queries, or
   - a separate `anchor_markers: Vec<(usize, usize)>` vector on `BufferTextStorage`.
   Resolution: during plan execution, inspect `buffer.rs` around `MarkerEntry` definition. If a separate vector is cleaner, use that.

2. **`Cell<PositionCache>` soundness.** Sound because `BufferTextStorage` is always behind `Rc<RefCell<…>>`; the outer `RefCell` guarantees no concurrent borrows, and `Cell<Copy>` is always safe. Verify at implementation time that no conversion path holds a borrow across a `pos_cache.set(..)` in a way that could re-enter.

3. **`ensure_gap` over-allocation on small buffers.** Adding 2 KB on every grow means a buffer starting from empty allocates 2 KB on first insert. For large numbers of tiny buffers this could matter. GNU accepts this; we accept it too.

## Self-review notes

- No placeholders / TBDs.
- Scope matches "Approach B" from the brainstorming discussion: gap sizing + `_both` mutations + O(1) boundary + position cache. Gap shrinking and GNU-name renames explicitly listed as non-goals.
- Each testing item corresponds to a specific design claim.
- Migration order is compile-safe at every step.

## Open questions for reviewer

- The distance heuristic is copied from GNU (`50 + 50 * markers_checked`). Should we keep those as named constants (`CHARPOS_DISTANCE_BASE`, `CHARPOS_DISTANCE_INCR`) or inline them? (Leaning named constants.)
- Should the auto-anchor-marker threshold (5000 chars) be a named constant too? (Yes — `POSITION_CACHE_MARKER_STRIDE = 5000`.)
- No changes planned to `insdel.rs` public API — OK?
