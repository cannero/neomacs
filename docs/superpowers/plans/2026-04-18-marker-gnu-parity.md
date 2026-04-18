# Marker GNU-Parity Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace NeoMacs's two-layer marker architecture (`Vec<MarkerEntry>` per buffer + global `TaggedHeap.marker_ptrs` + `MarkerData` with stale position cache) with a single-layer GNU-parity architecture where `MarkerData` is authoritative and each buffer owns an intrusive `next_marker` chain.

**Architecture:** `MarkerData` gains `bytepos`, `charpos`, `next_marker: *mut MarkerObj`, becoming the single source of truth. `BufferTextStorage` replaces its `Vec<MarkerEntry>` with `markers_head: *mut MarkerObj`. GC gains `unchain_dead_markers()` that walks each buffer's chain before sweep, matching GNU's `sweep_buffer` → `unchain_dead_markers` ordering. Pdump format bumps v25 → v26.

**Tech Stack:** Rust (stable 1.93.1), `cargo nextest` (never `cargo test`), `cargo` from project root.

**Reference spec:** `docs/superpowers/specs/2026-04-18-marker-gnu-parity-design.md`

**Test discipline:** Run `cargo nextest run -p neovm-core <selector>` after every code change. Commit only when the tree is green. Never skip nextest, never run `cargo test`.

---

## File structure

| File | Responsibility |
|------|---------------|
| `neovm-core/src/heap_types.rs` | `MarkerData` struct (gains fields) |
| `neovm-core/src/tagged/gc.rs` | `TaggedHeap`: remove `marker_ptrs`, add `unchain_dead_markers` |
| `neovm-core/src/buffer/buffer_text.rs` | Chain ops on `BufferTextStorage.markers_head` |
| `neovm-core/src/buffer/buffer.rs` | `kill_buffer_collect`, `MarkerEntry` struct removal |
| `neovm-core/src/buffer/insdel.rs` | Callers of `adjust_markers_for_insert/delete` |
| `neovm-core/src/emacs_core/marker.rs` | Reader/writer builtins; flip authoritative source |
| `neovm-core/src/emacs_core/pdump/types.rs` | `DumpMarker`, `DumpBuffer`, format version |
| `neovm-core/src/emacs_core/pdump/convert.rs` | Serialize/deserialize marker chains |

---

## Task dependency graph

```
T1 (MarkerData fields)  ─┐
T2 (markers_head field) ─┼─> T3 (chain helpers) ─> T4 (writer cutover) ─> T5 (adjust/advance cutover) ─> T6 (reader cutover) ─> T7 (delete Vec + MarkerEntry)
                                                                                                                                  │
                                                                                                                                  v
                                                                                         T8 (GC unchain + delete marker_ptrs) <───┘
                                                                                                     │
                                                                                                     v
                                                                                         T9 (kill-buffer simplification)
                                                                                                     │
                                                                                                     v
                                                                                         T10 (pdump format bump)
                                                                                                     │
                                                                                                     v
                                                                                         T11 (final verification)
```

Tasks T1 and T2 are independent and additive. T3 onwards are sequential.

---

### Task 1: Extend `MarkerData` with position + chain fields

**Files:**
- Modify: `neovm-core/src/heap_types.rs:285-291`

This is additive. The new fields are set to safe defaults (`0`, `null_mut()`). Existing readers keep using the old `position: Option<i64>` field; the new fields are unused until Task 6.

- [ ] **Step 1: Write the failing test**

Add this test to a new file `neovm-core/src/heap_types_marker_test.rs`:

```rust
use super::*;

#[test]
fn marker_data_new_fields_default() {
    let data = MarkerData {
        buffer: None,
        position: None,
        insertion_type: false,
        marker_id: None,
        bytepos: 0,
        charpos: 0,
        next_marker: std::ptr::null_mut(),
    };
    assert_eq!(data.bytepos, 0);
    assert_eq!(data.charpos, 0);
    assert!(data.next_marker.is_null());
}
```

Register the test module in `neovm-core/src/heap_types.rs` at the end of the file:

```rust
#[cfg(test)]
#[path = "heap_types_marker_test.rs"]
mod marker_test;
```

- [ ] **Step 2: Run the test and verify it fails (compile error: missing fields)**

Run: `cargo nextest run -p neovm-core heap_types::marker_test`
Expected: FAIL — compile error, fields `bytepos`, `charpos`, `next_marker` don't exist on `MarkerData`.

- [ ] **Step 3: Add the fields**

Edit `neovm-core/src/heap_types.rs` `MarkerData`:

```rust
#[derive(Clone, Debug)]
pub struct MarkerData {
    pub buffer: Option<BufferId>,
    pub position: Option<i64>,
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
    /// Byte offset in buffer (authoritative after T6; unused before).
    pub bytepos: usize,
    /// Char offset in buffer (authoritative after T6; unused before).
    pub charpos: usize,
    /// Intrusive link to next marker in the owning buffer's chain.
    /// `null` if not on a chain. GC sweep order: unchain_dead_markers
    /// walks these BEFORE sweep_objects frees unmarked markers.
    pub next_marker: *mut crate::tagged::header::MarkerObj,
}
```

- [ ] **Step 4: Fix the existing constructor in `neovm-core/src/emacs_core/marker.rs:75-87`**

Replace `make_marker_value_with_id`:

```rust
pub(crate) fn make_marker_value_with_id(
    buffer_id: Option<BufferId>,
    position: Option<i64>,
    insertion_type: bool,
    marker_id: Option<u64>,
) -> Value {
    Value::make_marker(crate::heap_types::MarkerData {
        buffer: buffer_id,
        position,
        insertion_type,
        marker_id,
        bytepos: 0,
        charpos: 0,
        next_marker: std::ptr::null_mut(),
    })
}
```

Also fix any other place that constructs `MarkerData` directly. Grep:
```
Grep: pattern "MarkerData\s*\{" path neovm-core/src
```
Every match must include the three new fields.

- [ ] **Step 5: Run the test and verify it passes**

Run: `cargo nextest run -p neovm-core heap_types::marker_test`
Expected: PASS.

Also run: `cargo check -p neovm-core`
Expected: clean.

- [ ] **Step 6: Run marker regression tests to confirm no drift**

Run: `cargo nextest run -p neovm-core emacs_core::marker_test`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add neovm-core/src/heap_types.rs neovm-core/src/heap_types_marker_test.rs neovm-core/src/emacs_core/marker.rs
git commit -m "marker: add bytepos/charpos/next_marker to MarkerData (additive)"
```

---

### Task 2: Add `markers_head` to `BufferTextStorage`

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs:46-63` (struct), `:~70-90` (Clone impl — search for Clone impl near top)

Additive. No callers yet touch it; it stays `null_mut()`.

- [ ] **Step 1: Add the field to `BufferTextStorage`**

Edit `neovm-core/src/buffer/buffer_text.rs` `BufferTextStorage` struct:

```rust
#[derive(Clone)]
struct BufferTextStorage {
    layout: BufferTextLayout,
    gap: GapBuffer,
    modified_tick: i64,
    chars_modified_tick: i64,
    save_modified_tick: i64,
    text_props: TextPropertyTable,
    markers: Vec<MarkerEntry>,
    /// Head of the intrusive per-buffer marker chain (GNU `buffer->own_text.markers`).
    /// Populated in T4+; stays `null` until then.
    markers_head: *mut crate::tagged::header::MarkerObj,
    pos_cache: Cell<PositionCache>,
    anchor_cache: RefCell<Vec<(usize, usize)>>,
    anchor_cache_key: Cell<(usize, usize)>,
}
```

`#[derive(Clone)]` will reject the raw pointer. Replace with a manual impl at the location of the existing `impl Clone for BufferText` (search `impl Clone for BufferText`). Keep the existing impl for `BufferText` unchanged (it clones the `Rc`). Add for `BufferTextStorage`:

```rust
impl Clone for BufferTextStorage {
    fn clone(&self) -> Self {
        Self {
            layout: self.layout.clone(),
            gap: self.gap.clone(),
            modified_tick: self.modified_tick,
            chars_modified_tick: self.chars_modified_tick,
            save_modified_tick: self.save_modified_tick,
            text_props: self.text_props.clone(),
            markers: self.markers.clone(),
            // Chain head intentionally not cloned: chain pointers are unique
            // per TaggedHeap; a cloned buffer starts with an empty chain and
            // rebuilds it via register_marker (T4+).
            markers_head: std::ptr::null_mut(),
            pos_cache: self.pos_cache.clone(),
            anchor_cache: self.anchor_cache.clone(),
            anchor_cache_key: self.anchor_cache_key.clone(),
        }
    }
}
```

Then remove the `#[derive(Clone)]` attribute from `BufferTextStorage`.

- [ ] **Step 2: Update every `BufferTextStorage { ... }` constructor to include the new field**

Grep: `BufferTextStorage\s*\{` in `neovm-core/src/buffer/buffer_text.rs`. For each, add `markers_head: std::ptr::null_mut(),`.

- [ ] **Step 3: Run compile check**

Run: `cargo check -p neovm-core`
Expected: clean.

- [ ] **Step 4: Run buffer and marker tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs
git commit -m "buffer_text: add markers_head chain head to BufferTextStorage"
```

---

### Task 3: Intrusive chain helpers on `BufferText` + unit tests

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs` (add helper methods after `marker_entries_snapshot`)
- Create: `neovm-core/src/buffer/buffer_text_chain_test.rs`

This adds low-level chain ops (splice at head, find-and-unlink, walk). Not yet wired to any caller.

- [ ] **Step 1: Write the failing test**

Create `neovm-core/src/buffer/buffer_text_chain_test.rs`:

```rust
use super::*;
use crate::heap_types::MarkerData;
use crate::tagged::gc::{TaggedHeap, set_tagged_heap};
use crate::tagged::header::MarkerObj;

fn alloc_marker_for_test(heap: &mut TaggedHeap) -> *mut MarkerObj {
    let tv = heap.alloc_marker(MarkerData {
        buffer: None,
        position: None,
        insertion_type: false,
        marker_id: None,
        bytepos: 0,
        charpos: 0,
        next_marker: std::ptr::null_mut(),
    });
    tv.as_veclike_ptr().unwrap() as *mut MarkerObj
}

#[test]
fn chain_splice_at_head_and_walk() {
    let mut heap = Box::new(TaggedHeap::new());
    set_tagged_heap(&mut heap);

    let bt = BufferText::new_empty();
    let m1 = alloc_marker_for_test(&mut heap);
    let m2 = alloc_marker_for_test(&mut heap);
    let m3 = alloc_marker_for_test(&mut heap);

    bt.chain_splice_at_head(m1);
    bt.chain_splice_at_head(m2);
    bt.chain_splice_at_head(m3);

    let walked = bt.chain_walk_collect();
    assert_eq!(walked, vec![m3, m2, m1]);
}

#[test]
fn chain_unlink_front_middle_back() {
    let mut heap = Box::new(TaggedHeap::new());
    set_tagged_heap(&mut heap);

    let bt = BufferText::new_empty();
    let m1 = alloc_marker_for_test(&mut heap);
    let m2 = alloc_marker_for_test(&mut heap);
    let m3 = alloc_marker_for_test(&mut heap);
    bt.chain_splice_at_head(m1);
    bt.chain_splice_at_head(m2);
    bt.chain_splice_at_head(m3);

    // Unlink middle
    bt.chain_unlink(m2);
    assert_eq!(bt.chain_walk_collect(), vec![m3, m1]);

    // Unlink head
    bt.chain_unlink(m3);
    assert_eq!(bt.chain_walk_collect(), vec![m1]);

    // Unlink last
    bt.chain_unlink(m1);
    assert_eq!(bt.chain_walk_collect(), vec![]);
}

#[test]
fn chain_unlink_absent_is_noop() {
    let mut heap = Box::new(TaggedHeap::new());
    set_tagged_heap(&mut heap);

    let bt = BufferText::new_empty();
    let m1 = alloc_marker_for_test(&mut heap);
    let absent = alloc_marker_for_test(&mut heap);
    bt.chain_splice_at_head(m1);

    bt.chain_unlink(absent);  // not in chain; must not panic or corrupt
    assert_eq!(bt.chain_walk_collect(), vec![m1]);
}
```

Register the module at the bottom of `neovm-core/src/buffer/buffer_text.rs`:

```rust
#[cfg(test)]
#[path = "buffer_text_chain_test.rs"]
mod chain_test;
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo nextest run -p neovm-core buffer::buffer_text::chain_test`
Expected: FAIL — methods `chain_splice_at_head`, `chain_unlink`, `chain_walk_collect` don't exist.

- [ ] **Step 3: Implement the helpers on `BufferText`**

Add after `marker_entries_snapshot` (around line 660):

```rust
/// Splice `marker` at the head of this buffer's marker chain.
/// `marker.next_marker` is overwritten with the old head.
/// Caller is responsible for setting `marker.buffer` / `marker.bytepos` /
/// `marker.charpos` — this helper only manipulates the chain.
pub fn chain_splice_at_head(&self, marker: *mut crate::tagged::header::MarkerObj) {
    let mut storage = self.storage.borrow_mut();
    let old_head = storage.markers_head;
    unsafe {
        (*marker).data.next_marker = old_head;
    }
    storage.markers_head = marker;
}

/// Unlink `marker` from this buffer's chain. Silent no-op if not present.
/// Does NOT clear `marker.buffer` / positions — caller owns semantic cleanup.
pub fn chain_unlink(&self, marker: *mut crate::tagged::header::MarkerObj) {
    let mut storage = self.storage.borrow_mut();
    // Walk with a prev tracker that points at the slot holding `curr`.
    let mut prev_slot: *mut *mut crate::tagged::header::MarkerObj =
        &mut storage.markers_head;
    unsafe {
        while !(*prev_slot).is_null() {
            let curr = *prev_slot;
            if curr == marker {
                *prev_slot = (*curr).data.next_marker;
                (*curr).data.next_marker = std::ptr::null_mut();
                return;
            }
            prev_slot = &mut (*curr).data.next_marker;
        }
    }
}

/// Walk the chain from head to tail, collecting raw pointers in order.
/// Test-only helper.
#[cfg(test)]
pub fn chain_walk_collect(&self) -> Vec<*mut crate::tagged::header::MarkerObj> {
    let storage = self.storage.borrow();
    let mut out = Vec::new();
    let mut curr = storage.markers_head;
    unsafe {
        while !curr.is_null() {
            out.push(curr);
            curr = (*curr).data.next_marker;
        }
    }
    out
}
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo nextest run -p neovm-core buffer::buffer_text::chain_test`
Expected: all 3 pass.

- [ ] **Step 5: Run full buffer tests to ensure no regressions**

Run: `cargo nextest run -p neovm-core buffer`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs neovm-core/src/buffer/buffer_text_chain_test.rs
git commit -m "buffer_text: add intrusive marker chain helpers (unused)"
```

---

### Task 4: Dual-write marker registration (writers populate both Vec and chain)

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs:539-594` (`register_marker`, `remove_marker`, `update_marker_insertion_type`)
- Modify: `neovm-core/src/emacs_core/marker.rs:89-112` (`make_registered_buffer_marker` — reach MarkerObj pointer for splicing)

This makes the writer path maintain the chain AND update `MarkerData.buffer/bytepos/charpos`. Readers still use the Vec until T6.

- [ ] **Step 1: Thread marker pointer through `register_marker`**

Modify `BufferText::register_marker` (line 539) to also splice into chain and update `MarkerData`:

```rust
pub fn register_marker(
    &self,
    marker_ptr: *mut crate::tagged::header::MarkerObj,
    buffer_id: BufferId,
    marker_id: u64,
    byte_pos: usize,
    char_pos: usize,
    insertion_type: InsertionType,
) {
    // Update MarkerData first so its fields are authoritative before the
    // chain ever exposes this marker.
    unsafe {
        (*marker_ptr).data.buffer = Some(buffer_id);
        (*marker_ptr).data.marker_id = Some(marker_id);
        (*marker_ptr).data.bytepos = byte_pos;
        (*marker_ptr).data.charpos = char_pos;
        (*marker_ptr).data.insertion_type = insertion_type == InsertionType::After;
    }
    self.chain_splice_at_head(marker_ptr);

    // Legacy Vec maintained for now; deleted in T7.
    let mut storage = self.storage.borrow_mut();
    storage.markers.push(MarkerEntry {
        id: marker_id,
        buffer_id,
        byte_pos,
        char_pos,
        insertion_type,
    });
}
```

- [ ] **Step 2: Update every caller of `register_marker` to pass the marker pointer**

Grep: `\.register_marker\(` in `neovm-core/src`. Known sites:
- `BufferManager::register_marker_id` at `neovm-core/src/buffer/buffer.rs:~3817`
- pdump load path (search `convert.rs` for `register_marker`)

For `BufferManager::register_marker_id`, change signature to take `marker_ptr`:

```rust
// in buffer.rs, was: pub fn register_marker_id(&mut self, buffer_id, marker_id, byte_pos, ins_type) -> ()
pub fn register_marker_id(
    &mut self,
    marker_ptr: *mut crate::tagged::header::MarkerObj,
    buffer_id: BufferId,
    marker_id: u64,
    byte_pos: usize,
    char_pos: usize,
    insertion_type: InsertionType,
) {
    if let Some(buffer) = self.buffers.get(&buffer_id) {
        buffer.text.register_marker(
            marker_ptr,
            buffer_id,
            marker_id,
            byte_pos,
            char_pos,
            insertion_type,
        );
    }
}
```

Note: the existing API probably doesn't carry `char_pos`. Inspect the callers — if `char_pos` isn't available, derive it via `buf.text.buf_bytepos_to_charpos(byte_pos)` at the caller site.

Update callers of `register_marker_id` (search `register_marker_id` in `neovm-core/src/emacs_core/marker.rs` — especially in `register_marker_in_buffers`) to pass the pointer:

```rust
// in marker.rs register_marker_in_buffers (~line 572), replace the existing
// create_marker + set_marker_id dance:
let marker_ptr = marker.as_veclike_ptr().unwrap() as *mut crate::tagged::header::MarkerObj;
let char_pos = /* compute from byte_pos via buf.text */;
buffers.register_marker_id(
    marker_ptr,
    buf_id,
    marker_id,
    byte_pos,
    char_pos,
    insertion_type,
);
```

Inspect the current shape of `register_marker_in_buffers` before editing; it may currently call `BufferManager::create_marker` (which allocates a new ID) followed by `set_marker_id`. The new flow should get the marker ID once and pass it + the pointer through.

- [ ] **Step 3: Update `remove_marker` to also unlink from chain**

```rust
pub fn remove_marker(&self, marker_id: u64) {
    // Find the MarkerObj pointer via chain walk BEFORE mutating storage,
    // so we can unlink. In T6+ we'll drop the Vec-based id lookup entirely.
    let marker_ptr: Option<*mut crate::tagged::header::MarkerObj> = {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        let mut found = None;
        unsafe {
            while !curr.is_null() {
                if (*curr).data.marker_id == Some(marker_id) {
                    found = Some(curr);
                    break;
                }
                curr = (*curr).data.next_marker;
            }
        }
        found
    };
    if let Some(ptr) = marker_ptr {
        self.chain_unlink(ptr);
        unsafe {
            (*ptr).data.buffer = None;
            (*ptr).data.bytepos = 0;
            (*ptr).data.charpos = 0;
        }
    }
    self.storage
        .borrow_mut()
        .markers
        .retain(|marker| marker.id != marker_id);
}
```

- [ ] **Step 4: Update `update_marker_insertion_type` to also update `MarkerData`**

Walk the chain and update `MarkerData.insertion_type` when a match is found, in addition to the existing Vec mutation.

```rust
pub fn update_marker_insertion_type(&self, marker_id: u64, insertion_type: InsertionType) {
    {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                if (*curr).data.marker_id == Some(marker_id) {
                    (*curr).data.insertion_type = insertion_type == InsertionType::After;
                    break;
                }
                curr = (*curr).data.next_marker;
            }
        }
    }
    let mut storage = self.storage.borrow_mut();
    let Some(marker) = storage
        .markers
        .iter_mut()
        .find(|marker| marker.id == marker_id)
    else {
        return;
    };
    marker.insertion_type = insertion_type;
}
```

- [ ] **Step 5: Run marker + buffer tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test`
Expected: all pass. If any regress, fix before proceeding.

- [ ] **Step 6: Run bootstrap tests**

Run: `cargo nextest run -p neovm-core emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs neovm-core/src/buffer/buffer.rs neovm-core/src/emacs_core/marker.rs
git commit -m "marker: dual-write register/unregister to chain and Vec"
```

---

### Task 5: Dual-write adjust/advance paths

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs:595-645` (`adjust_markers_for_insert`, `adjust_markers_for_delete`, `advance_markers_at`)

Chain walk replaces the Vec iteration in semantics, but Vec is still maintained for readers.

- [ ] **Step 1: Replace `adjust_markers_for_insert` body**

```rust
pub fn adjust_markers_for_insert(&self, insert_pos: usize, byte_len: usize, char_len: usize) {
    if byte_len == 0 {
        return;
    }
    // Chain-side update (MarkerData is authoritative in T6+).
    {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos > insert_pos {
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                } else if data.bytepos == insert_pos && data.insertion_type {
                    // insertion_type==true means "after" in GNU terms
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                }
                curr = data.next_marker;
            }
        }
    }
    // Vec-side update (deleted in T7).
    for marker in &mut self.storage.borrow_mut().markers {
        if marker.byte_pos > insert_pos {
            marker.byte_pos += byte_len;
            marker.char_pos += char_len;
        } else if marker.byte_pos == insert_pos && marker.insertion_type == InsertionType::After {
            marker.byte_pos += byte_len;
            marker.char_pos += char_len;
        }
    }
}
```

- [ ] **Step 2: Replace `adjust_markers_for_delete` body**

```rust
pub fn adjust_markers_for_delete(
    &self,
    start: usize,
    end: usize,
    start_char: usize,
    end_char: usize,
) {
    if start >= end {
        return;
    }
    let byte_len = end - start;
    let char_len = end_char - start_char;
    // Chain-side update.
    {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos >= end {
                    data.bytepos -= byte_len;
                    data.charpos -= char_len;
                } else if data.bytepos > start {
                    data.bytepos = start;
                    data.charpos = start_char;
                }
                curr = data.next_marker;
            }
        }
    }
    // Vec-side update.
    for marker in &mut self.storage.borrow_mut().markers {
        if marker.byte_pos >= end {
            marker.byte_pos -= byte_len;
            marker.char_pos -= char_len;
        } else if marker.byte_pos > start {
            marker.byte_pos = start;
            marker.char_pos = start_char;
        }
    }
}
```

- [ ] **Step 3: Replace `advance_markers_at` body**

```rust
pub fn advance_markers_at(&self, pos: usize, byte_len: usize, char_len: usize) {
    if byte_len == 0 {
        return;
    }
    {
        let storage = self.storage.borrow();
        let mut curr = storage.markers_head;
        unsafe {
            while !curr.is_null() {
                let data = &mut (*curr).data;
                if data.bytepos == pos {
                    data.bytepos += byte_len;
                    data.charpos += char_len;
                }
                curr = data.next_marker;
            }
        }
    }
    for marker in &mut self.storage.borrow_mut().markers {
        if marker.byte_pos == pos {
            marker.byte_pos += byte_len;
            marker.char_pos += char_len;
        }
    }
}
```

- [ ] **Step 4: Run the full marker + buffer + bootstrap test sweep**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs
git commit -m "marker: dual-write adjust/advance to chain and Vec"
```

---

### Task 6: Flip readers to `MarkerData` as source of truth

**Files:**
- Modify: `neovm-core/src/emacs_core/marker.rs:168-176, 193-223, 283-300, 232-254` and any other sites that read position or buffer

Replace `marker_entry(mid)` lookups with direct `MarkerData` reads.

- [ ] **Step 1: Replace `marker_position_value`, `marker_position_as_int_with_buffers`, `builtin_marker_position_in_buffers`**

The current pattern is "try marker_entry lookup, fall back to MarkerData.position". New pattern is "read MarkerData.charpos directly".

```rust
fn marker_position_value(v: &Value) -> Value {
    if !v.is_marker() {
        return Value::NIL;
    };
    let data = v.as_marker_data().unwrap();
    if data.buffer.is_none() {
        return Value::NIL;
    }
    Value::fixnum(data.charpos as i64 + 1)
}

pub(crate) fn marker_position_as_int_with_buffers(
    buffers: &BufferManager,
    v: &Value,
) -> Result<i64, Flow> {
    expect_marker("marker-position", v)?;

    // Special mark-marker: resolves via buffer's tracked mark-char.
    // The generic chain-based path below handles ordinary markers.
    if is_mark_marker(v) {
        if let Some(buf_id) = marker_buffer_id(v)
            && let Some(buf) = buffers.get(buf_id)
        {
            return match buf.mark_char() {
                Some(char_pos) => Ok(char_pos as i64 + 1),
                None => Err(signal(
                    "error",
                    vec![Value::string("Marker does not point anywhere")],
                )),
            };
        }
    }

    let data = v.as_marker_data().unwrap();
    if data.buffer.is_none() {
        return Err(signal(
            "error",
            vec![Value::string("Marker does not point anywhere")],
        ));
    }
    Ok(data.charpos as i64 + 1)
}

pub(crate) fn builtin_marker_position_in_buffers(
    _buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("marker-position", &args, 1)?;
    expect_marker("marker-position", &args[0])?;
    Ok(marker_position_value(&args[0]))
}
```

The `buffers: &BufferManager` argument on `builtin_marker_position_in_buffers` is now unused but kept to preserve the call signature. If the caller chain is small, drop it. Grep `builtin_marker_position_in_buffers` to see callers before changing the signature.

- [ ] **Step 2: Update any remaining `marker_entry` callers to read `MarkerData` directly**

Grep: `\.marker_entry\(` in `neovm-core/src`. Each callsite either becomes a direct `v.as_marker_data()` read or goes away entirely if it was just fetching a field we can now read from `MarkerData`.

- [ ] **Step 3: Update `marker_equal_hash_key_value` to use bytepos/charpos rather than the stale `position`**

```rust
pub(crate) fn marker_equal_hash_key_value(v: &Value) -> HashKey {
    if let Some(marker) = v.as_marker_data() {
        HashKey::Text(format!(
            "marker:{:?}:{}:{}",
            marker.buffer.map(|buffer| buffer.0),
            marker.charpos,
            marker.insertion_type
        ))
    } else {
        HashKey::Ptr(v.bits())
    }
}
```

- [ ] **Step 4: Run marker + buffer + bootstrap tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: all pass.

Also run the oracle compat tests:
Run: `cargo nextest run -p neovm-core --test compat_marker_semantics`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add neovm-core/src/emacs_core/marker.rs
git commit -m "marker: flip readers to MarkerData as authoritative source"
```

---

### Task 7: Delete `MarkerEntry` Vec and Vec-based helpers

**Files:**
- Modify: `neovm-core/src/buffer/buffer.rs` (delete `MarkerEntry` struct definition)
- Modify: `neovm-core/src/buffer/buffer_text.rs` (delete Vec field, Vec-only methods, Vec side of dual-writes)

At this point nothing reads the Vec. Delete it.

- [ ] **Step 1: Delete Vec side of dual-writes in T4/T5**

In `register_marker`: remove the `let mut storage = self.storage.borrow_mut(); storage.markers.push(...)` block.
In `remove_marker`: remove the trailing `.markers.retain(...)` block.
In `update_marker_insertion_type`: remove the Vec mutation block.
In `adjust_markers_for_insert`, `adjust_markers_for_delete`, `advance_markers_at`: remove the `for marker in &mut self.storage.borrow_mut().markers { ... }` block.

- [ ] **Step 2: Delete dead methods**

Remove from `BufferText` impl (`buffer_text.rs`):
- `pub fn marker_entry(&self, marker_id: u64) -> Option<MarkerEntry>` (lines ~567-574)
- `pub fn clear_markers(&self)` (~646-648)
- `pub fn remove_markers_for_buffers(&self, killed: &HashSet<BufferId>)` (~650-655)
- `pub fn marker_entries_snapshot(&self) -> Vec<MarkerEntry>` (~657-659)

Search callers of each (`Grep: marker_entry`, `Grep: clear_markers`, `Grep: remove_markers_for_buffers`, `Grep: marker_entries_snapshot`) and replace:
- `marker_entry` callers: should already be gone after T6. If any remain, rewrite them to read `MarkerData` directly.
- `clear_markers` callers: replace with chain walk-and-detach (done in T9).
- `remove_markers_for_buffers` callers: same.
- `marker_entries_snapshot` callers: likely only pdump. Will be replaced in T10.

Until T9/T10 land, convert callers to walk the chain inline. Example for the kill-buffer path (will be moved into T9 cleanly):

```rust
// in buffer.rs kill_buffer_collect, replace `clear_markers` call with:
let buffer = self.buffers.get(&id).unwrap();
buffer.text.chain_walk_mut(|m| unsafe {
    (*m).data.buffer = None;
    (*m).data.bytepos = 0;
    (*m).data.charpos = 0;
});
buffer.text.chain_clear_head();
```

This requires two new tiny helpers; add to `BufferText`:

```rust
pub fn chain_walk_mut(&self, mut f: impl FnMut(*mut crate::tagged::header::MarkerObj)) {
    let storage = self.storage.borrow();
    let mut curr = storage.markers_head;
    unsafe {
        while !curr.is_null() {
            let next = (*curr).data.next_marker;
            f(curr);
            curr = next;
        }
    }
}

pub fn chain_clear_head(&self) {
    self.storage.borrow_mut().markers_head = std::ptr::null_mut();
}
```

- [ ] **Step 3: Delete the Vec field and `MarkerEntry` struct**

Remove `markers: Vec<MarkerEntry>,` from `BufferTextStorage` and update its Clone impl to drop that field.

Delete `MarkerEntry` struct definition in `buffer.rs` (~lines 1299-1304). Remove `MarkerEntry` from the `pub use` / `pub(crate) use` lists in `buffer/mod.rs` if present.

- [ ] **Step 4: Fix compile errors**

Expect some stragglers — follow the compiler.

Run: `cargo check -p neovm-core`
Address each error.

- [ ] **Step 5: Run full marker + buffer + bootstrap tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "marker: delete MarkerEntry and Vec-based marker bookkeeping"
```

---

### Task 8: GC — `unchain_dead_markers` and delete `marker_ptrs`

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs:398, 928-939, 978-993, 1085-1102, 1315-1348`

Add `unchain_dead_markers()` between `mark_all` and `sweep_objects`, then remove `marker_ptrs` entirely.

- [ ] **Step 1: Add `unchain_dead_markers` method on `TaggedHeap`**

Place alongside other sweep helpers (after `sweep_cons` and before `sweep_objects`, around line 1312):

```rust
/// Walk each live buffer's marker chain and unlink any marker whose
/// `header.gc.marked` is false. Runs BEFORE `sweep_objects` frees them,
/// so the `marked` bit is read while the allocation is still live.
/// Mirrors GNU Emacs `sweep_buffer` → `unchain_dead_markers` ordering.
fn unchain_dead_markers(&mut self) {
    // Collect buffer pointers first to avoid aliasing issues with
    // `self.buffer_registry` while we mutate buffer-local state.
    let buffers: Vec<*mut BufferObj> = self
        .buffer_registry
        .values()
        .filter_map(|v| v.as_veclike_ptr().map(|p| p as *mut BufferObj))
        .collect();
    for buf in buffers {
        unsafe {
            let text = &(*buf).data.text;
            let mut prev_slot: *mut *mut MarkerObj =
                text.markers_head_slot_raw();
            while !(*prev_slot).is_null() {
                let curr = *prev_slot;
                if (*curr).header.gc.marked {
                    prev_slot = &mut (*curr).data.next_marker;
                } else {
                    // Splice out. The marker will be reclaimed by the
                    // regular `sweep_objects` pass; we just remove it
                    // from the chain so later adjust/kill ops don't
                    // dereference a freed object.
                    *prev_slot = (*curr).data.next_marker;
                    (*curr).data.next_marker = std::ptr::null_mut();
                }
            }
        }
    }
}
```

This requires a new helper on `BufferText` that exposes the raw slot address (needed because `unchain_dead_markers` is inside GC, outside `&self` borrow rules). Add to `BufferText`:

```rust
/// Raw `*mut *mut MarkerObj` pointing at the chain-head slot inside
/// this buffer's storage. ONLY for GC use — callers must hold the
/// TaggedHeap lock, and must not call this while any other storage
/// borrow is outstanding.
pub unsafe fn markers_head_slot_raw(&self) -> *mut *mut crate::tagged::header::MarkerObj {
    // RefCell::as_ptr gives us the underlying storage; from there we
    // project to the markers_head field. Keeping it unsafe because we
    // bypass RefCell's runtime borrow checks.
    let storage_ptr: *mut BufferTextStorage = self.storage.as_ptr();
    unsafe { &mut (*storage_ptr).markers_head as *mut _ }
}
```

Also resolve the exact `BufferObj` type path and `data.text` accessor by inspecting `neovm-core/src/tagged/header.rs` and `neovm-core/src/buffer/buffer.rs`. If `BufferObj.data.text` isn't the right path, use whichever field gives access to the underlying `BufferText`.

- [ ] **Step 2: Call `unchain_dead_markers` in `complete_collection`**

Modify `complete_collection` (line 1085):

```rust
pub(crate) fn complete_collection(&mut self) {
    self.mark_all();
    // Unchain dead markers from buffer chains BEFORE sweep_objects frees
    // them. Mirrors GNU sweep_buffer → unchain_dead_markers → block sweep.
    self.unchain_dead_markers();

    let cons_live_bytes = self.sweep_cons();
    let object_live_bytes = self.sweep_objects();
    self.live_bytes = cons_live_bytes.saturating_add(object_live_bytes);
    self.bytes_since_gc = 0;

    self.clear_dirty_owners();
    self.clear_dirty_writes();
}
```

- [ ] **Step 3: Remove `marker_ptrs.retain` from `sweep_objects`**

Delete the block at lines 1316-1325 (the `self.marker_ptrs.retain(|ptr| ...)` prune). `unchain_dead_markers` replaces it.

- [ ] **Step 4: Remove `marker_ptrs` field and uses**

- Delete the field declaration in `TaggedHeap`.
- Delete `self.marker_ptrs.push(ptr);` in `alloc_marker` (line 934).
- Delete `pub fn clear_markers_for_buffers<S>(&mut self, killed: ...)` (lines 978-993). All callers (search `clear_markers_for_buffers`) must be deleted as part of this task.
- Delete `marker_ptrs: Vec::new(),` in `TaggedHeap::new` and any other initializer.
- Delete any `Clone for TaggedHeap` handling of `marker_ptrs` (search).
- Delete any `Drop for TaggedHeap` handling that frees pointers via `marker_ptrs` (it should not exist — objects are freed via `sweep_objects` / Drop through `all_objects`, not `marker_ptrs`; confirm by inspection).

- [ ] **Step 5: Run GC tests**

Run: `cargo nextest run -p neovm-core tagged::gc`
Expected: all pass.

- [ ] **Step 6: Run bootstrap ASAN-adjacent test**

Run: `cargo nextest run -p neovm-core emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: PASS.

- [ ] **Step 7: Run marker tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test --test compat_marker_semantics`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "gc: unchain_dead_markers replaces marker_ptrs (GNU parity)"
```

---

### Task 9: Simplify `kill_buffer_collect`

**Files:**
- Modify: `neovm-core/src/buffer/buffer.rs:2919-2952`

Replace the two-step `clear_markers_for_buffers` + `clear_markers` cleanup with a single chain walk.

- [ ] **Step 1: Rewrite the marker cleanup inside `kill_buffer_collect`**

Find the existing marker-cleanup block (lines ~2928-2936):

```rust
// OLD:
with_tagged_heap(|heap| heap.clear_markers_for_buffers(&killed_set));
if kill_root {
    self.buffers.get(&id)?.text.clear_markers();
} else {
    self.buffers.get(&id)?.text.remove_markers_for_buffers(&killed_set);
}
```

Replace with:

```rust
// Walk each killed buffer's marker chain once: detach each marker
// (buffer/pos nulled) and clear the chain head. Mirrors GNU
// kill-buffer's marker cleanup.
for killed_id in &killed_ids {
    if let Some(buffer) = self.buffers.get(killed_id) {
        buffer.text.chain_walk_mut(|m| unsafe {
            (*m).data.buffer = None;
            (*m).data.bytepos = 0;
            (*m).data.charpos = 0;
            (*m).data.next_marker = std::ptr::null_mut();
        });
        buffer.text.chain_clear_head();
    }
}
```

Delete the `clear_markers_for_buffers` / `clear_markers` / `remove_markers_for_buffers` call sites that this replaces. Delete `kill_root` branch logic if it was only distinguishing the Vec-clearing behavior.

- [ ] **Step 2: Remove the now-dead `clear_markers_for_buffers` TaggedHeap method if T8 didn't already**

- [ ] **Step 3: Run buffer + marker + bootstrap tests**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/buffer/buffer.rs
git commit -m "buffer: simplify kill-buffer marker cleanup to chain walk"
```

---

### Task 10: Pdump format bump v25 → v26

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/types.rs:429-435, 461-464, 544, and format version constant`
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs:454-457, 851, and marker_entries_snapshot callers`

Drop `DumpMarkerEntry`. `DumpMarker` carries bytepos/charpos directly. `DumpBuffer.markers` becomes `Vec<DumpMarker>` in chain order.

- [ ] **Step 1: Find the current format version constant**

Grep: `DUMP_FORMAT_VERSION|V25|V26` in `neovm-core/src/emacs_core/pdump/`. Note the exact constant name and value.

- [ ] **Step 2: Update `DumpMarker` in `types.rs`**

```rust
#[derive(Serialize, Deserialize, ...)]  // existing derives
pub struct DumpMarker {
    pub buffer: Option<DumpBufferId>,  // existing fields
    pub insertion_type: bool,
    pub marker_id: Option<u64>,
    pub bytepos: usize,  // NEW
    pub charpos: usize,  // NEW
    // pub position: Option<i64>,  // REMOVED
}
```

Double-check by reading the file; preserve any existing derives and field order conventions.

- [ ] **Step 3: Delete `DumpMarkerEntry` struct**

```rust
// DELETE:
// pub struct DumpMarkerEntry { id, buffer_id, byte_pos, char_pos, insertion_type }
```

- [ ] **Step 4: Update `DumpBuffer.markers`**

```rust
pub struct DumpBuffer {
    // ... existing fields ...
    pub markers: Vec<DumpMarker>,  // was: Vec<DumpMarkerEntry>
    // ...
}
```

- [ ] **Step 5: Bump format version**

Change the version constant (whatever `DUMP_FORMAT_VERSION` is named) from its current value to the next one.

- [ ] **Step 6: Update `dump_marker_object` in `convert.rs:851`**

```rust
fn dump_marker_object(obj: &MarkerObj, ...) -> DumpMarker {
    DumpMarker {
        buffer: obj.data.buffer.map(...),
        insertion_type: obj.data.insertion_type,
        marker_id: obj.data.marker_id,
        bytepos: obj.data.bytepos,
        charpos: obj.data.charpos,
    }
}
```

Preserve the function signature and the exact mapping of `buffer` via whatever id-mapping helper is in use. Inspect before editing.

- [ ] **Step 7: Update marker load path in `convert.rs:454-457`**

Where markers are reconstructed during load, set the new fields:

```rust
let _ = marker.with_marker_data_mut(|data| {
    data.buffer = dump_marker.buffer.map(|b| load_buffer_id(b));
    data.insertion_type = dump_marker.insertion_type;
    data.marker_id = dump_marker.marker_id;
    data.bytepos = dump_marker.bytepos;
    data.charpos = dump_marker.charpos;
    data.next_marker = std::ptr::null_mut();  // spliced in next step
});
```

- [ ] **Step 8: Update DumpBuffer load path to reconstruct the chain**

Where `DumpBuffer.markers` is consumed on load, replace any `Vec<DumpMarkerEntry>` iteration with iteration over `Vec<DumpMarker>` that, for each entry, allocates the MarkerObj (already dumped/loaded as a separate heap object under the marker_id) and splices it into the buffer's chain at tail.

Grep for `DumpMarkerEntry` uses on the load side; each must be rewritten.

If the current format already emits a separate heap-side marker object AND a per-buffer DumpMarkerEntry (duplicating the marker_id mapping), the new format only needs the per-buffer entries — the heap MarkerObjs are reconstructed from the buffer load. Consolidate.

- [ ] **Step 9: Update the `DumpBuffer` dump path**

Where `DumpBuffer` is built (search for `DumpBuffer {` construction in `convert.rs`), replace `markers: buffer.text.marker_entries_snapshot().iter().map(...).collect()` (or similar) with a chain walk:

```rust
let mut markers = Vec::new();
buffer.text.chain_walk_mut(|m| unsafe {
    markers.push(DumpMarker {
        buffer: (*m).data.buffer.map(|b| dump_buffer_id(b)),
        insertion_type: (*m).data.insertion_type,
        marker_id: (*m).data.marker_id,
        bytepos: (*m).data.bytepos,
        charpos: (*m).data.charpos,
    });
});
```

Note that `chain_walk_mut` walks head→tail, so the resulting `Vec<DumpMarker>` preserves chain order; the load-side splice must splice at tail to preserve order.

- [ ] **Step 10: Run pdump + bootstrap tests**

Run: `cargo nextest run -p neovm-core emacs_core::pdump`
Expected: all pass.

Run: `cargo nextest run -p neovm-core emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: PASS. If the test cache is stale (mismatched format version), delete the cache directory and re-run — the spec allows discarding old dumps.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "pdump: bump v25→v26, DumpMarker carries bytepos/charpos directly"
```

---

### Task 11: Final verification

**Files:** none (tests only)

- [ ] **Step 1: Full marker semantics run**

Run: `cargo nextest run -p neovm-core buffer emacs_core::marker_test`
Expected: all pass.

- [ ] **Step 2: Oracle compat tests**

Run: `cargo nextest run --test compat_marker_semantics`
Expected: all pass.

- [ ] **Step 3: Bootstrap + pdump reload**

Run: `cargo nextest run -p neovm-core emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form`
Expected: PASS.

- [ ] **Step 4: ASAN run of the bootstrap test**

Build the ASAN binary:

```bash
mv rust-toolchain.toml rust-toolchain.toml.bak
RUSTC_WRAPPER= \
  PATH=~/.rustup/toolchains/nightly-2026-03-12-x86_64-unknown-linux-gnu/bin:$PATH \
  RUSTFLAGS="-Z sanitizer=address" \
  cargo -Zbuild-std nextest run -p neovm-core --target x86_64-unknown-linux-gnu \
    emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form --no-run
mv rust-toolchain.toml.bak rust-toolchain.toml
```

Find the ASAN test binary (latest `target/x86_64-unknown-linux-gnu/debug/deps/neovm_core-*` executable) and run:

```bash
RUST_MIN_STACK=67108864 \
ASAN_OPTIONS="detect_leaks=0:abort_on_error=0:halt_on_error=1:print_stacktrace=1" \
./target/x86_64-unknown-linux-gnu/debug/deps/neovm_core-<hash> \
  emacs_core::load::tests::cached_bootstrap_reload_evaluates_full_advice_remove_member_form \
  --exact --nocapture > /tmp/asan-final.txt 2>&1
echo "exit=$?"
grep -a -E "ERROR|SUMMARY:" /tmp/asan-final.txt | head -5
```

Expected: `exit=0`, no ASAN ERROR lines (only elisp source doc-strings containing the word "ERROR", like `(fn ERROR BASE)` are fine).

- [ ] **Step 5: Spot-check broader suite**

Run: `cargo nextest run -p neovm-core emacs_core::load::tests --no-fail-fast 2>&1 | tail -20`

Expected: counts match pre-refactor baseline. A small handful of pre-existing failures is acceptable; verify they were already failing before this plan started by checking the comments on commits `5e1ba959f` and earlier. No NEW failures introduced.

- [ ] **Step 6: Confirm no dangling references**

Grep for dead identifiers that should now be gone:

```
Grep: MarkerEntry path neovm-core
Grep: marker_ptrs path neovm-core
Grep: marker_entries_snapshot path neovm-core
Grep: clear_markers_for_buffers path neovm-core
Grep: \.marker_entry\( path neovm-core
```

Expected: zero matches for each. If any remain, fix in a cleanup commit.

- [ ] **Step 7: Final commit (if any spot-fixes)**

```bash
git add -A
git commit -m "marker: final cleanup post-GNU-parity refactor" || echo "no cleanup needed"
```

---

## Post-plan summary checklist

- [ ] `TaggedHeap.marker_ptrs` field does not exist.
- [ ] `MarkerEntry` struct does not exist.
- [ ] `BufferTextStorage` has exactly one marker field: `markers_head: *mut MarkerObj`.
- [ ] `MarkerData` carries `bytepos`, `charpos`, `next_marker`, `marker_id`, `buffer`, `insertion_type`; `position: Option<i64>` is gone if any readers still used it — if any remain, leave it for a follow-up and document in commit message.
- [ ] `complete_collection` runs `mark_all` → `unchain_dead_markers` → `sweep_cons` → `sweep_objects`.
- [ ] `kill-buffer` walks the buffer's chain once to detach markers.
- [ ] pdump format version bumped; `DumpMarker` carries bytepos/charpos; `DumpMarkerEntry` is gone.
- [ ] All existing marker tests pass.
- [ ] ASAN on bootstrap test is clean.
