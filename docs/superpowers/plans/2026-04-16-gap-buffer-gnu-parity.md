# Gap Buffer GNU-Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close performance divergences between NeoMacs's gap buffer and GNU Emacs's `insdel.c` + `marker.c` by adopting GNU-matching gap sizing, pre-computed `(nchars, nbytes)` mutation hot paths, O(1) char-boundary checks, and a marker-anchored position cache on `BufferText`.

**Architecture:** `GapBuffer` stays low-level (raw bytes + gap movement). New `_both` variants accept pre-computed char counts; wrappers preserve the existing API. `BufferText` gains a `PositionCache` (`Cell<PositionCache>`) and `buf_charpos_to_bytepos` / `buf_bytepos_to_charpos` methods that bracket queries via anchors (BEG, GPT, Z), the existing `markers: Vec<MarkerEntry>` chain, the last-query cache, and a new `anchor_cache: Vec<(charpos, bytepos)>` auto-populated every 5000 chars walked.

**Tech Stack:** Rust (stable 1.93.1), `cargo nextest` for tests, existing `neovm-core` crate.

**Spec reference:** `docs/superpowers/specs/2026-04-16-gap-buffer-gnu-parity-design.md`

**Test runner convention (project-wide):** use `cargo nextest run -p neovm-core <test_name>` per `feedback_cargo_nextest`. Never `cargo test`. Use plain `cargo check -p neovm-core` for compile validation during development (not `--release`).

---

## Task 1: Raise gap-sizing constants to GNU defaults

**Rationale:** Current `DEFAULT_GAP_SIZE = 64` / `MIN_GAP_GROW = 64` cause ~30× more reallocations than GNU's 2000 B default grow. This is the single largest perf win.

**Files:**
- Modify: `neovm-core/src/buffer/gap_buffer.rs` (lines 14-20, `ensure_gap` at 549-566, `new_with_multibyte` at 63-74, `from_emacs_bytes` at 77-94)
- Test: `neovm-core/src/buffer/gap_buffer_test.rs`

- [ ] **Step 1.1: Write the failing test**

Append to `neovm-core/src/buffer/gap_buffer_test.rs`:

```rust
#[test]
fn new_buffer_has_gnu_default_gap_size() {
    let gb = GapBuffer::new();
    assert!(
        gb.gap_size() >= 2000,
        "expected gap_size >= 2000, got {}",
        gb.gap_size()
    );
}

#[test]
fn ensure_gap_grows_beyond_requested_minimum() {
    let mut gb = GapBuffer::new();
    // Fill current gap completely so the next ensure_gap must actually grow.
    let filler = vec![b'a'; gb.gap_size()];
    gb.insert_emacs_bytes(0, &filler);
    assert_eq!(gb.gap_size(), 0);
    gb.ensure_gap(1);
    // GNU adds GAP_BYTES_DFL beyond caller's request.
    assert!(
        gb.gap_size() >= 2000,
        "expected ensure_gap(1) to grow gap to >= 2000, got {}",
        gb.gap_size()
    );
}
```

- [ ] **Step 1.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core new_buffer_has_gnu_default_gap_size ensure_gap_grows_beyond_requested_minimum`
Expected: both FAIL with `expected gap_size >= 2000, got 64`.

- [ ] **Step 1.3: Change constants**

In `neovm-core/src/buffer/gap_buffer.rs`, replace lines 14-20:

```rust
/// Default extra gap bytes to pre-allocate on any growth.
/// Matches GNU Emacs `GAP_BYTES_DFL` (`src/buffer.h:205`).
const GAP_BYTES_DFL: usize = 2000;

/// Floor for the gap after shrinking — not enforced today because we don't
/// shrink yet, but kept as a named constant to match GNU's `GAP_BYTES_MIN`
/// (`src/buffer.h:210`).
#[allow(dead_code)]
const GAP_BYTES_MIN: usize = 20;
```

- [ ] **Step 1.4: Update `new_with_multibyte`**

In `neovm-core/src/buffer/gap_buffer.rs` at `new_with_multibyte` (lines 63-74), replace `DEFAULT_GAP_SIZE` with `GAP_BYTES_DFL`:

```rust
pub fn new_with_multibyte(multibyte: bool) -> Self {
    Self {
        buf: vec![0u8; GAP_BYTES_DFL],
        multibyte,
        gap_start: 0,
        gap_end: GAP_BYTES_DFL,
        gap_start_chars: 0,
        total_chars: 0,
        gap_start_bytes: 0,
        total_bytes: 0,
    }
}
```

- [ ] **Step 1.5: Update `from_emacs_bytes`**

Replace the `let gap = DEFAULT_GAP_SIZE;` line (around line 78) with `let gap = GAP_BYTES_DFL;`.

- [ ] **Step 1.6: Update `ensure_gap`**

Replace the body of `ensure_gap` (lines 549-566):

```rust
pub fn ensure_gap(&mut self, min_size: usize) {
    if self.gap_size() >= min_size {
        return;
    }
    // GNU insdel.c:483 (`make_gap_larger`): add GAP_BYTES_DFL beyond the
    // caller's requested need so a run of sequential inserts is amortized
    // O(1) rather than paying realloc on every ~64 bytes.
    let need = min_size - self.gap_size();
    let grow = need.saturating_add(GAP_BYTES_DFL);
    let old_gap_end = self.gap_end;
    let after_gap_len = self.buf.len() - old_gap_end;

    self.buf.resize(self.buf.len() + grow, 0);

    if after_gap_len > 0 {
        self.buf
            .copy_within(old_gap_end..old_gap_end + after_gap_len, old_gap_end + grow);
    }
    self.gap_end += grow;
}
```

- [ ] **Step 1.7: Run tests to verify pass**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/gap_buffer/)'`
Expected: all `gap_buffer_test` tests PASS, including the two new ones.

- [ ] **Step 1.8: Commit**

```bash
git add neovm-core/src/buffer/gap_buffer.rs neovm-core/src/buffer/gap_buffer_test.rs
git commit -m "gap_buffer: raise gap-sizing constants to GNU defaults

GNU Emacs uses GAP_BYTES_DFL = 2000 (buffer.h:205) and adds that amount
beyond each grow request, so sequential inserts pay realloc only every
~2KB rather than every ~64 bytes. Match that exactly."
```

---

## Task 2: Make `is_char_boundary` O(1) via CHAR_HEAD_P bit check

**Rationale:** Current implementation scans from byte 0 on every boundary check — used in `debug_assert!` paths that turn insert/delete into quadratic code under debug builds.

**Files:**
- Modify: `neovm-core/src/buffer/gap_buffer.rs` (the private `is_char_boundary` method at lines 649-659 and the free helper `is_emacs_char_boundary` at lines 758-772)
- Test: `neovm-core/src/buffer/gap_buffer_test.rs`

- [ ] **Step 2.1: Write the failing test**

Append to `neovm-core/src/buffer/gap_buffer_test.rs`:

```rust
#[test]
fn is_char_boundary_matches_oracle_on_large_multibyte_buffer() {
    // Build a large mixed ASCII + CJK buffer.
    let mut s = String::new();
    for i in 0..2000 {
        if i % 3 == 0 {
            s.push_str("日本語");
        } else {
            s.push_str("hello");
        }
    }
    let gb = GapBuffer::from_str(&s);
    // Oracle: walk char boundaries from 0, mark each one.
    let bytes: Vec<u8> = (0..gb.len()).map(|i| gb.byte_at(i)).collect();
    let mut oracle_boundary = vec![false; gb.len() + 1];
    oracle_boundary[0] = true;
    let mut p = 0usize;
    while p < bytes.len() {
        let (_, len) = crate::emacs_core::emacs_char::string_char(&bytes[p..]);
        p += len;
        oracle_boundary[p] = true;
    }
    // Spot-check 500 positions: the private method is not accessible, but
    // byte_to_char asserts on boundary internally, so round-trip via it.
    // For positions that SHOULD be boundaries, byte_to_char must not panic.
    for i in (0..gb.len()).step_by(gb.len() / 500 + 1) {
        if oracle_boundary[i] {
            // Should not panic.
            let _ = gb.byte_to_char(i);
        }
    }
}
```

- [ ] **Step 2.2: Run to verify the test passes today (it's a regression guard)**

Run: `cargo nextest run -p neovm-core is_char_boundary_matches_oracle_on_large_multibyte_buffer`
Expected: PASS (this is a correctness regression guard — the test must pass both before and after the refactor).

- [ ] **Step 2.3: Replace the private `is_char_boundary` with an O(1) version**

In `neovm-core/src/buffer/gap_buffer.rs`, replace the method at lines 649-659:

```rust
/// Check whether `pos` falls on a logical Emacs-character boundary.
/// O(1): single-byte bit test matching GNU's `CHAR_HEAD_P` (character.h).
fn is_char_boundary(&self, pos: usize) -> bool {
    if !self.multibyte || pos == 0 || pos >= self.len() {
        return true;
    }
    // Multibyte trailing bytes have the form 10xxxxxx (0x80..=0xBF).
    // Any other byte value is a character head.
    (self.byte_at(pos) & 0xC0) != 0x80
}
```

- [ ] **Step 2.4: Replace the free helper `is_emacs_char_boundary`**

In `neovm-core/src/buffer/gap_buffer.rs`, replace the function at lines 758-772:

```rust
#[inline]
fn is_emacs_char_boundary(bytes: &[u8], byte_pos: usize, multibyte: bool) -> bool {
    if byte_pos > bytes.len() {
        return false;
    }
    if !multibyte || byte_pos == 0 || byte_pos == bytes.len() {
        return true;
    }
    // Same CHAR_HEAD_P bit test as the method.
    (bytes[byte_pos] & 0xC0) != 0x80
}
```

- [ ] **Step 2.5: Run full gap_buffer test suite**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/gap_buffer/)'`
Expected: all PASS.

- [ ] **Step 2.6: Commit**

```bash
git add neovm-core/src/buffer/gap_buffer.rs neovm-core/src/buffer/gap_buffer_test.rs
git commit -m "gap_buffer: O(1) char-boundary check via CHAR_HEAD_P

The boundary test was scanning from byte 0 on every call. GNU uses a
single-bit test on the candidate byte — trailing bytes match 10xxxxxx,
anything else is a char head. Switch to that, eliminating the O(n) scan."
```

---

## Task 3: Add `insert_emacs_bytes_both` accepting pre-computed `nchars`

**Rationale:** Current `insert_emacs_bytes` re-scans the payload on every insertion to count chars. Callers that already know the count (which is most callers in `insdel.rs`) should be able to skip the scan.

**Files:**
- Modify: `neovm-core/src/buffer/gap_buffer.rs` (`insert_emacs_bytes` at 349-374)
- Test: `neovm-core/src/buffer/gap_buffer_test.rs`

- [ ] **Step 3.1: Write the failing test**

Append to `neovm-core/src/buffer/gap_buffer_test.rs`:

```rust
#[test]
fn insert_emacs_bytes_both_matches_scanning_variant() {
    let bytes = "Hello, 日本語!".as_bytes().to_vec();
    let nchars = crate::emacs_core::emacs_char::chars_in_multibyte(&bytes);

    let mut a = GapBuffer::new();
    a.insert_emacs_bytes(0, &bytes);

    let mut b = GapBuffer::new();
    b.insert_emacs_bytes_both(0, &bytes, nchars);

    assert_eq!(a.to_string(), b.to_string());
    assert_eq!(a.char_count(), b.char_count());
    assert_eq!(a.emacs_byte_len(), b.emacs_byte_len());
    assert_eq!(a.gpt(), b.gpt());
    assert_eq!(a.gpt_byte(), b.gpt_byte());
}
```

- [ ] **Step 3.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core insert_emacs_bytes_both_matches_scanning_variant`
Expected: FAIL (compile error — method does not exist).

- [ ] **Step 3.3: Extract `_both` variant; have `insert_emacs_bytes` delegate**

In `neovm-core/src/buffer/gap_buffer.rs`, replace `insert_emacs_bytes` (lines 349-374) with:

```rust
/// Insert raw Emacs bytes at logical byte position `pos`.
///
/// Convenience wrapper that counts characters in `bytes`. If the caller
/// already knows `nchars`, prefer `insert_emacs_bytes_both`.
pub fn insert_emacs_bytes(&mut self, pos: usize, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let nchars = emacs_char_count_bytes(bytes, self.multibyte);
    self.insert_emacs_bytes_both(pos, bytes, nchars);
}

/// Insert raw Emacs bytes at logical byte position `pos`, given the
/// pre-computed character count.
///
/// # Safety (logical)
///
/// `nchars` **must** equal `chars_in_multibyte(bytes)` (or `bytes.len()` in
/// unibyte mode). Passing a wrong value corrupts the char/byte counters.
///
/// Mirrors GNU `insert_1_both` (`src/insdel.c:891`).
pub fn insert_emacs_bytes_both(&mut self, pos: usize, bytes: &[u8], nchars: usize) {
    assert!(
        pos <= self.len(),
        "insert_emacs_bytes_both: position {pos} out of range (len {})",
        self.len()
    );
    if bytes.is_empty() {
        return;
    }
    debug_assert!(
        pos == self.len() || self.is_char_boundary(pos),
        "insert_emacs_bytes_both: position {pos} is not on an Emacs character boundary"
    );
    debug_assert_eq!(
        nchars,
        emacs_char_count_bytes(bytes, self.multibyte),
        "insert_emacs_bytes_both: caller-supplied nchars mismatches actual"
    );

    let inserted_bytes = bytes.len();
    self.move_gap_to(pos);
    self.ensure_gap(inserted_bytes);

    self.buf[self.gap_start..self.gap_start + inserted_bytes].copy_from_slice(bytes);
    self.gap_start += inserted_bytes;
    self.gap_start_chars += nchars;
    self.total_chars += nchars;
    self.gap_start_bytes += inserted_bytes;
    self.total_bytes += inserted_bytes;
}
```

- [ ] **Step 3.4: Run tests**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/gap_buffer/)'`
Expected: all PASS.

- [ ] **Step 3.5: Commit**

```bash
git add neovm-core/src/buffer/gap_buffer.rs neovm-core/src/buffer/gap_buffer_test.rs
git commit -m "gap_buffer: add insert_emacs_bytes_both taking pre-computed nchars

Mirrors GNU insert_1_both (insdel.c:891). The scan-counting wrapper stays
as insert_emacs_bytes so existing call sites keep working; callers that
already know the char count can skip the extra scan."
```

---

## Task 4: Add `delete_range_both` accepting pre-computed `nchars`

**Files:**
- Modify: `neovm-core/src/buffer/gap_buffer.rs` (`delete_range` at 399-431)
- Test: `neovm-core/src/buffer/gap_buffer_test.rs`

- [ ] **Step 4.1: Write the failing test**

Append to `neovm-core/src/buffer/gap_buffer_test.rs`:

```rust
#[test]
fn delete_range_both_matches_scanning_variant() {
    let source = "Hello, 日本語 world!";
    let mut a = GapBuffer::from_str(source);
    let mut b = GapBuffer::from_str(source);

    // Delete the CJK span: bytes 7..=15 (3 chars * 3 bytes).
    let from = 7;
    let to = 7 + "日本語".len();

    // Compute nchars for the deleted slice via oracle.
    let mut tmp = Vec::new();
    b.copy_bytes_to(from, to, &mut tmp);
    let nchars = crate::emacs_core::emacs_char::chars_in_multibyte(&tmp);

    a.delete_range(from, to);
    b.delete_range_both(from, to, nchars);

    assert_eq!(a.to_string(), b.to_string());
    assert_eq!(a.char_count(), b.char_count());
    assert_eq!(a.emacs_byte_len(), b.emacs_byte_len());
}
```

- [ ] **Step 4.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core delete_range_both_matches_scanning_variant`
Expected: FAIL (method does not exist).

- [ ] **Step 4.3: Extract `_both` variant; have `delete_range` delegate**

In `neovm-core/src/buffer/gap_buffer.rs`, replace `delete_range` (lines 399-431) with:

```rust
/// Delete the logical byte range `[start, end)`.
///
/// Wrapper that counts deleted chars. Prefer `delete_range_both` if the
/// caller already knows the count.
pub fn delete_range(&mut self, start: usize, end: usize) {
    assert!(start <= end, "delete_range: start ({start}) > end ({end})");
    assert!(
        end <= self.len(),
        "delete_range: end ({end}) > len ({})",
        self.len()
    );
    if start == end {
        return;
    }
    // Count deleted chars by peeking at the soon-to-be-deleted region.
    // This requires a temporary slice; the scan is the cost we're avoiding
    // when callers know the count.
    let mut tmp = Vec::with_capacity(end - start);
    self.copy_bytes_to(start, end, &mut tmp);
    let nchars = emacs_char_count_bytes(&tmp, self.multibyte);
    self.delete_range_both(start, end, nchars);
}

/// Delete the logical byte range `[start, end)`, given pre-computed char
/// count of the region.
///
/// Mirrors GNU `del_range_2` (`src/insdel.c:1991`).
pub fn delete_range_both(&mut self, start: usize, end: usize, nchars: usize) {
    assert!(
        start <= end,
        "delete_range_both: start ({start}) > end ({end})"
    );
    assert!(
        end <= self.len(),
        "delete_range_both: end ({end}) > len ({})",
        self.len()
    );
    if start == end {
        return;
    }
    debug_assert!(
        self.is_char_boundary(start),
        "delete_range_both: start ({start}) is not on an Emacs character boundary"
    );
    debug_assert!(
        end == self.len() || self.is_char_boundary(end),
        "delete_range_both: end ({end}) is not on an Emacs character boundary"
    );

    self.move_gap_to(start);
    let deleted_bytes = end - start;
    // After move_gap_to(start), bytes [start, end) now live at
    // buf[gap_end .. gap_end + deleted_bytes]; extend the gap to swallow them.
    self.gap_end += deleted_bytes;
    self.total_chars -= nchars;
    self.total_bytes -= deleted_bytes;
}
```

- [ ] **Step 4.4: Run tests**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/gap_buffer/)'`
Expected: all PASS.

- [ ] **Step 4.5: Commit**

```bash
git add neovm-core/src/buffer/gap_buffer.rs neovm-core/src/buffer/gap_buffer_test.rs
git commit -m "gap_buffer: add delete_range_both taking pre-computed nchars

Mirrors GNU del_range_2 (insdel.c:1991). delete_range keeps its current
signature as a scan-then-delegate wrapper."
```

---

## Task 5: Add `move_gap_both` accepting pre-computed `charpos`

**Rationale:** `move_gap_to` currently calls `emacs_char_count_bytes` on the moved region, doubling the memmove cost. Callers that know the target char position (from a char-to-byte conversion) can pass it in.

**Files:**
- Modify: `neovm-core/src/buffer/gap_buffer.rs` (`move_gap_to` at 504-545)
- Test: `neovm-core/src/buffer/gap_buffer_test.rs`

- [ ] **Step 5.1: Write the failing test**

Append to `neovm-core/src/buffer/gap_buffer_test.rs`:

```rust
#[test]
fn move_gap_both_matches_scanning_variant() {
    let source = "Hello, 日本語 world!";
    let mut a = GapBuffer::from_str(source);
    let mut b = GapBuffer::from_str(source);

    // Move the gap to a position inside the CJK run.
    let bytepos = 7 + "日".len(); // byte position of '本'
    let charpos = a.byte_to_char(bytepos);

    a.move_gap_to(bytepos);
    b.move_gap_both(bytepos, charpos);

    assert_eq!(a.to_string(), b.to_string());
    assert_eq!(a.gpt(), b.gpt());
    assert_eq!(a.gpt_byte(), b.gpt_byte());
    assert_eq!(a.char_count(), b.char_count());
}
```

- [ ] **Step 5.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core move_gap_both_matches_scanning_variant`
Expected: FAIL (method does not exist).

- [ ] **Step 5.3: Extract `_both` variant; have `move_gap_to` delegate**

In `neovm-core/src/buffer/gap_buffer.rs`, replace `move_gap_to` (lines 504-545) with:

```rust
/// Move the gap so that `gap_start == pos`.
///
/// Wrapper that computes the char delta by scanning moved bytes. Prefer
/// `move_gap_both` when the caller knows the target char position.
pub fn move_gap_to(&mut self, pos: usize) {
    assert!(
        pos <= self.len(),
        "move_gap_to: position {pos} out of range (len {})",
        self.len()
    );
    if pos == self.gap_start {
        return;
    }
    // Derive the target char position by scanning moved bytes. The scan is
    // exactly what move_gap_both lets the caller skip.
    let charpos = if pos < self.gap_start {
        let moved = emacs_char_count_bytes(&self.buf[pos..self.gap_start], self.multibyte);
        self.gap_start_chars - moved
    } else {
        let moved =
            emacs_char_count_bytes(&self.buf[self.gap_end..self.gap_end + (pos - self.gap_start)], self.multibyte);
        self.gap_start_chars + moved
    };
    self.move_gap_both(pos, charpos);
}

/// Move the gap so that `gap_start == bytepos` and `gap_start_chars == charpos`.
///
/// Mirrors GNU `move_gap_both` (`src/insdel.c:88`).
pub fn move_gap_both(&mut self, bytepos: usize, charpos: usize) {
    assert!(
        bytepos <= self.len(),
        "move_gap_both: bytepos {bytepos} out of range (len {})",
        self.len()
    );
    if bytepos == self.gap_start {
        return;
    }
    let gap = self.gap_size();

    if bytepos < self.gap_start {
        let count = self.gap_start - bytepos;
        self.buf.copy_within(bytepos..bytepos + count, bytepos + gap);
        self.gap_start = bytepos;
        self.gap_end = bytepos + gap;
        self.gap_start_chars = charpos;
        self.gap_start_bytes = bytepos;
    } else {
        let count = bytepos - self.gap_start;
        let src_start = self.gap_end;
        let dst_start = self.gap_start;
        self.buf.copy_within(src_start..src_start + count, dst_start);
        self.gap_start = bytepos;
        self.gap_end = bytepos + gap;
        self.gap_start_chars = charpos;
        self.gap_start_bytes = bytepos;
    }
}
```

- [ ] **Step 5.4: Run tests**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/gap_buffer/)'`
Expected: all PASS.

- [ ] **Step 5.5: Commit**

```bash
git add neovm-core/src/buffer/gap_buffer.rs neovm-core/src/buffer/gap_buffer_test.rs
git commit -m "gap_buffer: add move_gap_both taking pre-computed charpos

Mirrors GNU move_gap_both (insdel.c:88). move_gap_to keeps its current
signature as a scan-then-delegate wrapper."
```

---

## Task 6: Thread `nchars` through `insdel.rs` to call `_both` variants

**Rationale:** The core mutation driver already knows (or can cheaply derive) `nchars` when it calls into `GapBuffer`. Route it through the `_both` entry points to avoid the redundant scan.

**Files:**
- Modify: `neovm-core/src/buffer/insdel.rs`
- (No new tests — existing `insdel.rs` tests + buffer integration tests cover correctness.)

- [ ] **Step 6.1: Identify call sites**

Run: `cargo check -p neovm-core` — should compile unchanged.
Then: `grep -n "gap\.insert_emacs_bytes\|gap\.delete_range\|gap\.move_gap_to" neovm-core/src/buffer/insdel.rs neovm-core/src/buffer/buffer_text.rs`

Expected output (example — exact lines may vary):
```
neovm-core/src/buffer/insdel.rs:<N>:    storage.gap.insert_emacs_bytes(...)
neovm-core/src/buffer/insdel.rs:<M>:    storage.gap.delete_range(...)
neovm-core/src/buffer/buffer_text.rs:196:    storage.gap.insert_emacs_bytes(pos, bytes);
neovm-core/src/buffer/buffer_text.rs:205:    storage.gap.delete_range(start, end);
```

For each such call site, decide:
- If the calling function has `nchars` / `char_len` already in scope → switch to `_both`.
- Otherwise, count it once (via `chars_in_multibyte` in multibyte mode, or `bytes.len()` in unibyte) and pass through.

- [ ] **Step 6.2: Replace insertion call sites in `insdel.rs`**

For each `storage.gap.insert_emacs_bytes(pos, bytes)` call in `insdel.rs`:

```rust
// Before:
storage.gap.insert_emacs_bytes(pos, bytes);

// After:
let nchars = if storage.gap.is_multibyte() {
    crate::emacs_core::emacs_char::chars_in_multibyte(bytes)
} else {
    bytes.len()
};
storage.gap.insert_emacs_bytes_both(pos, bytes, nchars);
```

If `nchars` is already computed for `record_char_modification` in the same function, reuse that binding instead of recomputing.

- [ ] **Step 6.3: Replace deletion call sites in `insdel.rs`**

For each `storage.gap.delete_range(from, to)` call, if the function already computes `char_len` (it does at lines 159, 226, 235 per earlier grep), reuse it:

```rust
// Before:
storage.gap.delete_range(from, to);

// After:
storage.gap.delete_range_both(from, to, char_len);
```

If `char_len` is computed *after* the delete (because it reads back text), restructure: snapshot the chars by calling `char_count()` before and after the `_both` call, or compute from a copied slice before deletion.

- [ ] **Step 6.4: Run full buffer test suite**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/buffer/)'`
Expected: all PASS (no behavior change, just call routing).

- [ ] **Step 6.5: Commit**

```bash
git add neovm-core/src/buffer/insdel.rs
git commit -m "insdel: route mutations through gap_buffer _both variants

Callers already know nchars (computed for record_char_modification); pass
it through instead of making GapBuffer rescan the payload."
```

---

## Task 7: Add `PositionCache` + `buf_charpos_to_bytepos` on `BufferText`

**Rationale:** Core of approach B. Cached bracketed search replaces the current O(n) linear scan in `GapBuffer::char_to_byte`.

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs` (add `PositionCache`, field on `BufferTextStorage`, method on `BufferText`)
- Test: `neovm-core/src/buffer/buffer_text_test.rs`

- [ ] **Step 7.1: Write failing correctness test**

Append to `neovm-core/src/buffer/buffer_text_test.rs`:

```rust
#[test]
fn buf_charpos_to_bytepos_matches_oracle() {
    // 100KB multibyte buffer, mixed ASCII + CJK.
    let mut s = String::new();
    for i in 0..5000 {
        if i % 2 == 0 {
            s.push_str("hello ");
        } else {
            s.push_str("日本語 ");
        }
    }
    let text = BufferText::from_str(&s);

    // Oracle: contiguous bytes → char_to_byte_pos.
    let mut bytes = Vec::new();
    text.copy_bytes_to(0, text.len(), &mut bytes);

    for &cp in &[0usize, 1, 50, 500, 5000, 12345, text.char_count() - 1, text.char_count()] {
        let got = text.buf_charpos_to_bytepos(cp);
        let expected = crate::emacs_core::emacs_char::char_to_byte_pos(&bytes, cp);
        assert_eq!(
            got, expected,
            "charpos {cp}: buf_charpos_to_bytepos returned {got}, oracle said {expected}"
        );
    }
}
```

- [ ] **Step 7.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core buf_charpos_to_bytepos_matches_oracle`
Expected: FAIL (method does not exist).

- [ ] **Step 7.3: Add `PositionCache` struct and field**

In `neovm-core/src/buffer/buffer_text.rs`, change the imports at the top:

```rust
use std::cell::{Cell, RefCell};
```

Add the struct near the top of the file (after `BufferTextLayout`):

```rust
/// Last successful char↔byte conversion. Reused on a subsequent query if
/// `chars_modified_tick` has not advanced.
///
/// Mirrors GNU's `best_below` / `best_above` + modiff-gated reuse in
/// `marker.c::buf_charpos_to_bytepos`.
#[derive(Clone, Copy, Default)]
struct PositionCache {
    /// `chars_modified_tick` when this entry was stored. 0 = invalid.
    modiff: i64,
    charpos: usize,
    bytepos: usize,
}
```

Update `BufferTextStorage`:

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
    /// Interior-mutable position cache; read paths update it without needing
    /// `borrow_mut()` on the outer `RefCell`.
    pos_cache: Cell<PositionCache>,
}
```

Update every `BufferTextStorage { ... }` literal in the file (in `new`, `from_str`, `from_dump`) to include `pos_cache: Cell::new(PositionCache::default())`.

- [ ] **Step 7.4: Add the conversion method**

Append to the `impl BufferText` block (anywhere near `char_to_byte`):

```rust
/// Convert a character position to a logical Emacs byte offset using the
/// anchor-bracketed cached search. Mirrors GNU `buf_charpos_to_bytepos`
/// (`src/marker.c:167`).
pub fn buf_charpos_to_bytepos(&self, target: usize) -> usize {
    let storage = self.storage.borrow();
    let total_chars = storage.gap.char_count();
    let total_bytes = storage.gap.emacs_byte_len();
    let modiff = storage.chars_modified_tick;

    // Clamp out-of-range like the existing char_to_byte helper.
    if target >= total_chars {
        return storage.gap.len();
    }

    // Unibyte fast path: char == byte, no scan needed.
    if total_chars == total_bytes {
        return target;
    }

    // Initialise bracketing anchors (below, above) as (charpos, bytepos).
    let mut best_below: (usize, usize) = (0, 0);
    let mut best_above: (usize, usize) = (total_chars, total_bytes);

    // GPT anchor.
    let gpt = storage.gap.gpt();
    let gpt_byte = storage.gap.gpt_byte();
    consider_anchor(target, (gpt, gpt_byte), &mut best_below, &mut best_above);

    // Last-query cache anchor.
    let cached = storage.pos_cache.get();
    if cached.modiff == modiff && cached.modiff != 0 {
        consider_anchor(
            target,
            (cached.charpos, cached.bytepos),
            &mut best_below,
            &mut best_above,
        );
    }

    // Marker-chain anchors with early bail-out.
    let mut distance: usize = POSITION_DISTANCE_BASE;
    for m in &storage.markers {
        consider_anchor(target, (m.char_pos, m.byte_pos), &mut best_below, &mut best_above);
        if best_above.0 - target < distance || target - best_below.0 < distance {
            break;
        }
        distance = distance.saturating_add(POSITION_DISTANCE_INCR);
    }

    // Decide which bracket is closer and scan from there.
    let result = if target - best_below.0 <= best_above.0 - target {
        scan_forward(&storage.gap, best_below, target)
    } else {
        scan_backward(&storage.gap, best_above, target)
    };

    storage.pos_cache.set(PositionCache {
        modiff,
        charpos: target,
        bytepos: result,
    });
    result
}
```

Add private helpers at the bottom of the file (after the last `impl` block):

```rust
/// GNU `marker.c:162` — initial bracket-bail distance.
const POSITION_DISTANCE_BASE: usize = 50;
/// GNU `marker.c:162` — bracket-bail distance grows by this per marker checked.
const POSITION_DISTANCE_INCR: usize = 50;

fn consider_anchor(
    target: usize,
    anchor: (usize, usize),
    best_below: &mut (usize, usize),
    best_above: &mut (usize, usize),
) {
    if anchor.0 <= target && anchor.0 > best_below.0 {
        *best_below = anchor;
    }
    if anchor.0 >= target && anchor.0 < best_above.0 {
        *best_above = anchor;
    }
}

/// Walk forward from `anchor` to reach `target` chars, returning the bytepos.
fn scan_forward(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut cp, mut bp) = anchor;
    while cp < target {
        // Read one char at bp; advance.
        // `char_code_at` returns Option<u32>, but we already clamped target
        // to total_chars, so bp must be in range here.
        let byte0 = gap.byte_at(bp);
        let (_, len) = if gap.is_multibyte() {
            let mut tmp = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
            let available = (gap.len() - bp).min(tmp.len());
            for (i, slot) in tmp[..available].iter_mut().enumerate() {
                *slot = gap.byte_at(bp + i);
            }
            crate::emacs_core::emacs_char::string_char(&tmp[..available])
        } else {
            (byte0 as u32, 1)
        };
        bp += len;
        cp += 1;
    }
    bp
}

/// Walk backward from `anchor` to reach `target` chars, returning the bytepos.
fn scan_backward(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut cp, mut bp) = anchor;
    while cp > target {
        // Walk backwards one char: find previous char head by scanning back.
        if !gap.is_multibyte() {
            bp -= 1;
            cp -= 1;
            continue;
        }
        let mut prev = bp - 1;
        while prev > 0 && (gap.byte_at(prev) & 0xC0) == 0x80 {
            prev -= 1;
        }
        bp = prev;
        cp -= 1;
    }
    bp
}
```

- [ ] **Step 7.5: Run tests**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and (test(/buffer_text/) or test(buf_charpos_to_bytepos))'`
Expected: all PASS.

- [ ] **Step 7.6: Add cache-invalidation test**

Append to `neovm-core/src/buffer/buffer_text_test.rs`:

```rust
#[test]
fn buf_charpos_to_bytepos_invalidates_on_mutation() {
    let mut text = BufferText::from_str("abc");
    let first = text.buf_charpos_to_bytepos(2);
    assert_eq!(first, 2);

    // Insert at pos 0 — changes byte-at-charpos-2 mapping.
    text.insert_str(0, "é"); // é is 2 bytes in UTF-8
    let second = text.buf_charpos_to_bytepos(2);
    // After insertion, charpos 2 should be at bytepos 3 ("éa" = 2+1 bytes).
    assert_eq!(second, 3);
    assert_ne!(first, second, "cache returned stale bytepos after mutation");
}
```

Run: `cargo nextest run -p neovm-core buf_charpos_to_bytepos_invalidates_on_mutation`
Expected: PASS.

- [ ] **Step 7.7: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs neovm-core/src/buffer/buffer_text_test.rs
git commit -m "buffer_text: add buf_charpos_to_bytepos with anchor-bracketed cache

Mirrors GNU marker.c::buf_charpos_to_bytepos. Anchors from BEG, GPT, Z,
the marker chain, and a modiff-gated last-query cache. Scans from the
closer bracket instead of linearly from the start of the segment."
```

---

## Task 8: Add `buf_bytepos_to_charpos` (symmetric)

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs`
- Test: `neovm-core/src/buffer/buffer_text_test.rs`

- [ ] **Step 8.1: Write the failing test**

Append to `neovm-core/src/buffer/buffer_text_test.rs`:

```rust
#[test]
fn buf_bytepos_to_charpos_matches_oracle() {
    let mut s = String::new();
    for i in 0..5000 {
        if i % 2 == 0 { s.push_str("hello "); } else { s.push_str("日本語 "); }
    }
    let text = BufferText::from_str(&s);

    let mut bytes = Vec::new();
    text.copy_bytes_to(0, text.len(), &mut bytes);

    for &bp in &[0usize, 1, 50, 500, 5000, 12345, text.len() - 1, text.len()] {
        // Oracle only valid on char boundaries — snap bp down to one.
        let mut bp_snapped = bp;
        while bp_snapped > 0 && bp_snapped < bytes.len() && (bytes[bp_snapped] & 0xC0) == 0x80 {
            bp_snapped -= 1;
        }
        let got = text.buf_bytepos_to_charpos(bp_snapped);
        let expected = crate::emacs_core::emacs_char::byte_to_char_pos(&bytes, bp_snapped);
        assert_eq!(got, expected, "bytepos {bp_snapped}");
    }
}
```

- [ ] **Step 8.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core buf_bytepos_to_charpos_matches_oracle`
Expected: FAIL (method does not exist).

- [ ] **Step 8.3: Implement the method**

Append to the `impl BufferText` block:

```rust
/// Convert a byte position to a character position. Symmetric to
/// `buf_charpos_to_bytepos` — shares the same anchor + cache machinery.
pub fn buf_bytepos_to_charpos(&self, target: usize) -> usize {
    let storage = self.storage.borrow();
    let total_chars = storage.gap.char_count();
    let total_bytes = storage.gap.emacs_byte_len();
    let modiff = storage.chars_modified_tick;

    if target >= total_bytes {
        return total_chars;
    }
    if total_chars == total_bytes {
        return target;
    }

    let mut best_below: (usize, usize) = (0, 0); // (bytepos, charpos)
    let mut best_above: (usize, usize) = (total_bytes, total_chars);

    let gpt = storage.gap.gpt();
    let gpt_byte = storage.gap.gpt_byte();
    consider_anchor_byte(target, (gpt_byte, gpt), &mut best_below, &mut best_above);

    let cached = storage.pos_cache.get();
    if cached.modiff == modiff && cached.modiff != 0 {
        consider_anchor_byte(
            target,
            (cached.bytepos, cached.charpos),
            &mut best_below,
            &mut best_above,
        );
    }

    let mut distance: usize = POSITION_DISTANCE_BASE;
    for m in &storage.markers {
        consider_anchor_byte(target, (m.byte_pos, m.char_pos), &mut best_below, &mut best_above);
        if best_above.0 - target < distance || target - best_below.0 < distance {
            break;
        }
        distance = distance.saturating_add(POSITION_DISTANCE_INCR);
    }

    let result = if target - best_below.0 <= best_above.0 - target {
        scan_forward_bytes(&storage.gap, best_below, target)
    } else {
        scan_backward_bytes(&storage.gap, best_above, target)
    };

    storage.pos_cache.set(PositionCache {
        modiff,
        charpos: result,
        bytepos: target,
    });
    result
}
```

Add the helpers at the bottom of the file:

```rust
fn consider_anchor_byte(
    target: usize,
    anchor: (usize, usize), // (bytepos, charpos)
    best_below: &mut (usize, usize),
    best_above: &mut (usize, usize),
) {
    if anchor.0 <= target && anchor.0 > best_below.0 {
        *best_below = anchor;
    }
    if anchor.0 >= target && anchor.0 < best_above.0 {
        *best_above = anchor;
    }
}

fn scan_forward_bytes(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut bp, mut cp) = anchor;
    while bp < target {
        if !gap.is_multibyte() {
            bp += 1;
            cp += 1;
            continue;
        }
        let mut tmp = [0u8; crate::emacs_core::emacs_char::MAX_MULTIBYTE_LENGTH];
        let available = (gap.len() - bp).min(tmp.len());
        for (i, slot) in tmp[..available].iter_mut().enumerate() {
            *slot = gap.byte_at(bp + i);
        }
        let (_, len) = crate::emacs_core::emacs_char::string_char(&tmp[..available]);
        bp += len;
        cp += 1;
    }
    cp
}

fn scan_backward_bytes(gap: &GapBuffer, anchor: (usize, usize), target: usize) -> usize {
    let (mut bp, mut cp) = anchor;
    while bp > target {
        if !gap.is_multibyte() {
            bp -= 1;
            cp -= 1;
            continue;
        }
        let mut prev = bp - 1;
        while prev > 0 && (gap.byte_at(prev) & 0xC0) == 0x80 {
            prev -= 1;
        }
        bp = prev;
        cp -= 1;
    }
    cp
}
```

- [ ] **Step 8.4: Run tests**

Run: `cargo nextest run -p neovm-core buf_bytepos_to_charpos_matches_oracle`
Expected: PASS.

- [ ] **Step 8.5: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs neovm-core/src/buffer/buffer_text_test.rs
git commit -m "buffer_text: add buf_bytepos_to_charpos (symmetric)

Same anchor + cache machinery as buf_charpos_to_bytepos; walks from
the closer bracket instead of segment start."
```

---

## Task 9: Auto-anchor cache vector + insertion when scan is long

**Rationale:** GNU inserts a scratch marker every 5000 chars walked during conversion so future nearby queries are O(1). `MarkerEntry` in NeoMacs is Lisp-visible (has `id: u64`, appears in `register_marker`/`marker_entry`), so we use a separate vector to avoid polluting Lisp state.

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs`
- Test: `neovm-core/src/buffer/buffer_text_test.rs`

- [ ] **Step 9.1: Write the failing test**

Append to `neovm-core/src/buffer/buffer_text_test.rs`:

```rust
#[test]
fn long_scan_populates_anchor_cache() {
    // 10 000+ multibyte chars, no existing markers.
    let mut s = String::new();
    for _ in 0..10_000 {
        s.push_str("日");
    }
    let text = BufferText::from_str(&s);

    assert_eq!(text.anchor_cache_len(), 0);

    // Query a position > 5000 chars into the buffer, forcing a long scan.
    let _ = text.buf_charpos_to_bytepos(8000);

    assert!(
        text.anchor_cache_len() > 0,
        "expected auto-anchor to have been inserted after long scan"
    );
}
```

- [ ] **Step 9.2: Run to verify failure**

Run: `cargo nextest run -p neovm-core long_scan_populates_anchor_cache`
Expected: FAIL (method does not exist).

- [ ] **Step 9.3: Add the `anchor_cache` field**

In `neovm-core/src/buffer/buffer_text.rs`, update `BufferTextStorage`:

```rust
struct BufferTextStorage {
    // ... existing fields ...
    pos_cache: Cell<PositionCache>,
    /// Internal (non-Lisp) anchor positions populated on long scans.
    /// Invalidated wholesale when `chars_modified_tick` advances.
    anchor_cache: RefCell<Vec<(usize, usize)>>,
    /// `chars_modified_tick` value at last anchor-cache check. When stale,
    /// `anchor_cache` is cleared on next read.
    anchor_cache_modiff: Cell<i64>,
}
```

Update every `BufferTextStorage { ... }` literal to include:
```rust
anchor_cache: RefCell::new(Vec::new()),
anchor_cache_modiff: Cell::new(0),
```

- [ ] **Step 9.4: Add a test accessor**

Append to `impl BufferText`:

```rust
#[cfg(test)]
pub fn anchor_cache_len(&self) -> usize {
    self.storage.borrow().anchor_cache.borrow().len()
}
```

- [ ] **Step 9.5: Wire the auto-insertion threshold**

Add a constant near the other position constants:

```rust
/// Insert a scratch anchor when a conversion scan walks more than this many
/// chars. Mirrors GNU `marker.c:238-241` (the 5000-char threshold).
const POSITION_ANCHOR_STRIDE: usize = 5000;
```

Modify `buf_charpos_to_bytepos` — invalidate the anchor cache on modiff mismatch, consider anchors during bracketing, and push the result if the walked distance is large.

Replace the `buf_charpos_to_bytepos` body from Task 7 with the extended version:

```rust
pub fn buf_charpos_to_bytepos(&self, target: usize) -> usize {
    let storage = self.storage.borrow();
    let total_chars = storage.gap.char_count();
    let total_bytes = storage.gap.emacs_byte_len();
    let modiff = storage.chars_modified_tick;

    if target >= total_chars {
        return storage.gap.len();
    }
    if total_chars == total_bytes {
        return target;
    }

    // Invalidate the anchor cache if the buffer changed since last use.
    if storage.anchor_cache_modiff.get() != modiff {
        storage.anchor_cache.borrow_mut().clear();
        storage.anchor_cache_modiff.set(modiff);
    }

    let mut best_below: (usize, usize) = (0, 0);
    let mut best_above: (usize, usize) = (total_chars, total_bytes);

    let gpt = storage.gap.gpt();
    let gpt_byte = storage.gap.gpt_byte();
    consider_anchor(target, (gpt, gpt_byte), &mut best_below, &mut best_above);

    let cached = storage.pos_cache.get();
    if cached.modiff == modiff && cached.modiff != 0 {
        consider_anchor(
            target,
            (cached.charpos, cached.bytepos),
            &mut best_below,
            &mut best_above,
        );
    }

    for &(cp, bp) in storage.anchor_cache.borrow().iter() {
        consider_anchor(target, (cp, bp), &mut best_below, &mut best_above);
    }

    let mut distance: usize = POSITION_DISTANCE_BASE;
    for m in &storage.markers {
        consider_anchor(target, (m.char_pos, m.byte_pos), &mut best_below, &mut best_above);
        if best_above.0 - target < distance || target - best_below.0 < distance {
            break;
        }
        distance = distance.saturating_add(POSITION_DISTANCE_INCR);
    }

    let walked_below = target.saturating_sub(best_below.0);
    let walked_above = best_above.0.saturating_sub(target);
    let result = if walked_below <= walked_above {
        scan_forward(&storage.gap, best_below, target)
    } else {
        scan_backward(&storage.gap, best_above, target)
    };
    let walked = walked_below.min(walked_above);

    if walked > POSITION_ANCHOR_STRIDE {
        storage.anchor_cache.borrow_mut().push((target, result));
    }

    storage.pos_cache.set(PositionCache {
        modiff,
        charpos: target,
        bytepos: result,
    });
    result
}
```

Apply the symmetric change to `buf_bytepos_to_charpos`:
- Same anchor-cache invalidation block.
- Consider `anchor_cache` entries as `(bp, cp)` via `consider_anchor_byte`.
- Push `(result, target)` to `anchor_cache` when walked > `POSITION_ANCHOR_STRIDE`.

- [ ] **Step 9.6: Run tests**

Run: `cargo nextest run -p neovm-core -E 'package(neovm-core) and (test(/buffer_text/) or test(long_scan))'`
Expected: all PASS.

- [ ] **Step 9.7: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs neovm-core/src/buffer/buffer_text_test.rs
git commit -m "buffer_text: auto-insert anchors every 5000 chars walked

Mirrors GNU marker.c:238-241. Uses a private anchor_cache vector rather
than MarkerEntry to avoid polluting Lisp-visible marker state. Cleared
wholesale on chars_modified_tick bumps."
```

---

## Task 10: Delegate `BufferText::char_to_byte` / `byte_to_char` to the cached path

**Rationale:** With the cached conversion methods working, existing callers (editfns, xdisp, search, etc.) start benefiting transparently.

**Files:**
- Modify: `neovm-core/src/buffer/buffer_text.rs` (`byte_to_char` at 229-231, `char_to_byte` at 233-235)
- (No new test — relies on the existing large `buffer_text_test.rs` + `buffer_test.rs` corpus, plus the two new oracle tests from Tasks 7 & 8.)

- [ ] **Step 10.1: Replace delegations**

In `neovm-core/src/buffer/buffer_text.rs`, replace `byte_to_char` (line 229), `char_to_byte` (line 233), `emacs_byte_to_char` (line 237), and `char_to_emacs_byte` (line 241):

```rust
pub fn byte_to_char(&self, byte_pos: usize) -> usize {
    self.buf_bytepos_to_charpos(byte_pos)
}

pub fn char_to_byte(&self, char_pos: usize) -> usize {
    self.buf_charpos_to_bytepos(char_pos)
}

pub fn emacs_byte_to_char(&self, byte_pos: usize) -> usize {
    // In current NeoMacs, storage bytes == Emacs bytes, so this is an alias
    // for buf_bytepos_to_charpos. If the two ever diverge, this function is
    // the place to do the extra translation first.
    self.buf_bytepos_to_charpos(byte_pos)
}

pub fn char_to_emacs_byte(&self, char_pos: usize) -> usize {
    self.buf_charpos_to_bytepos(char_pos)
}
```

Leave `storage_byte_to_emacs_byte` and `emacs_byte_to_storage_byte` delegating to `GapBuffer` — those are O(1) clamp operations, not conversions.

- [ ] **Step 10.2: Run full test suite (catch any caller regression)**

Run: `cargo nextest run -p neovm-core` (no filter — whole crate).
Expected: all PASS. Pre-existing failures (the ~735 compat_* oracle tests noted in CLAUDE.md) are allowed; this task must not add new failures.

Quick-pass check:

```bash
cargo nextest run -p neovm-core -E 'package(neovm-core) and (test(/buffer/) or test(/editfns/) or test(/xdisp/))' 2>&1 | tail -20
```
Expected: no new failures vs baseline.

- [ ] **Step 10.3: Commit**

```bash
git add neovm-core/src/buffer/buffer_text.rs
git commit -m "buffer_text: route char_to_byte/byte_to_char through position cache

Callers (editfns, xdisp, search, etc.) now pick up the anchor-bracketed
cached conversion instead of the segment-linear scan in GapBuffer."
```

---

## Post-plan validation

After Task 10, run the full buffer-related test sweep one more time:

```bash
cargo nextest run -p neovm-core -E 'package(neovm-core) and test(/buffer/)' 2>&1 | tail -10
cargo nextest run -p neovm-core -E 'package(neovm-core) and (test(/insdel/) or test(/editfns/) or test(/xdisp/) or test(/search/))' 2>&1 | tail -10
```

Compare failure count against the baseline (pre-Task-1 state). Expected: zero regressions in buffer-local tests; any pre-existing compat_* failures unchanged.

## Performance validation (optional, not committed)

Write a throwaway `examples/gap_buffer_bench.rs` that measures:
- 1 M sequential ASCII chars appended to an initially-empty buffer (target: ≥3× speedup).
- 1 M sequential CJK (3-byte) chars appended (target: ≥3× speedup).
- 1 M random `char_to_byte` queries over a 1 MB multibyte buffer (target: ≥10× speedup).

Run pre-refactor (via `git stash` or a clean clone) and post-refactor. Record timings in the PR description. Do not commit the bench.

---

## Self-review notes

Spec coverage check:

| Spec section | Plan task |
|---|---|
| Gap sizing constants | Task 1 |
| `_both` variants on gap buffer | Tasks 3, 4, 5 |
| O(1) `is_char_boundary` | Task 2 |
| Thread nchars through callers | Task 6 |
| `PositionCache` + `buf_charpos_to_bytepos` | Task 7 |
| `buf_bytepos_to_charpos` (symmetric) | Task 8 |
| Auto-anchor insertion | Task 9 (risk resolved: separate `anchor_cache` vector) |
| Delegate `BufferText::byte_to_char` / `char_to_byte` | Task 10 |
| Unit tests | All tasks (inline TDD) |
| Perf validation | Post-plan section |

No placeholders. Types used in later tasks (`PositionCache`, `consider_anchor`, `scan_forward`, `anchor_cache`) are defined in the earlier task where they first appear.
