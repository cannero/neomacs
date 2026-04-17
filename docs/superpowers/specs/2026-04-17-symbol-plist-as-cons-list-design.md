# `LispSymbol::plist` as Cons List — Design

**Date:** 2026-04-17
**Status:** Approved
**Scope:** `neovm-core/src/emacs_core/symbol.rs` (primary), plus `builtins/symbols.rs`, `pdump/convert.rs`, `pdump/types.rs`, and the `Obarray::trace_roots` path.

## Motivation

`LispSymbol::plist` is currently `FxHashMap<SymId, Value>`. GNU stores it as `Lisp_Object plist` — a Lisp cons list of alternating key/value pairs (`(prop1 val1 prop2 val2 …)`). The HashMap shape cannot preserve GNU semantics across six fronts:

1. **Insertion order lost.** GNU iterates the plist in list order. `FxHashMap` iterates in arbitrary hash order.
2. **Duplicate keys silently collapsed.** GNU allows `(a 1 a 2)`; `plist-get` returns the first match. HashMap drops one.
3. **Shared-structure identity broken.** `(eq (symbol-plist 'foo) saved)` must be `t` across repeated calls in GNU. NeoMacs synthesizes a fresh cons list each call.
4. **Destructive mutation leak.** `(nconc (symbol-plist 'foo) '(x 1))` and `(push pair (symbol-plist 'foo))` modify the plist in GNU; NeoMacs mutations of the synthesized list don't reach the HashMap.
5. **Non-symbol keys unsupported.** GNU plists accept any Lisp value as a key (`cl-getf`, string keys, etc.). NeoMacs keyspace is `SymId`.
6. **`setplist` structural loss.** GNU stores whatever pointer you pass; NeoMacs parses to pairs, losing duplicates, ordering, and improper-list tails.

Most NeoMacs Lisp today uses `put` / `get` over unique-key plists, so the divergence is often hidden. Code that uses `cl-lib` macros, calls `setplist` directly, or manipulates `symbol-plist` destructively hits the divergence.

This refactor switches `LispSymbol::plist` to `Value` (a Lisp cons list), matching GNU `struct Lisp_Symbol::s.plist` (`lisp.h:820`). The project already has:
- `LispString::plist: Value` using the same shape.
- `OverlayData::plist: Value` plus `plist_get_eq` / `plist_put_eq` helpers in `buffer/overlay.rs`.

We reuse those helpers rather than inventing new ones.

## Goals

- `LispSymbol::plist: Value` (Lisp cons list), replacing `FxHashMap<SymId, Value>`.
- `(symbol-plist SYM)` returns the actual cons pointer stored on the symbol — two calls are `eq`.
- `(setplist SYM LIST)` stores `LIST` directly — no parsing / re-encoding.
- `(put SYM PROP VAL)` / `(get SYM PROP)` semantics unchanged from the caller's perspective; they go through cons-list walking via `plist_get_eq` / `plist_put_eq` (already used by overlay and image plists).
- Insertion order, duplicate keys, destructive mutation, and non-symbol keys all match GNU.
- GC trace walks the single `Value` root.

## Non-goals

- Changing `LispString::plist` or `OverlayData::plist` (already `Value`).
- Performance optimization. Cons-list `plist-get` is O(n) where HashMap was O(1). Plists are typically 2-10 entries; matches GNU's own cost.
- Plist lookup with `equal` semantics for non-eq keys (e.g., `lax-plist-get`) — that's a separate builtin. The core field change supports it; any missing builtin lands elsewhere.

## Architecture

### Before

```rust
pub struct LispSymbol {
    pub name: NameId,
    pub flags: SymbolFlags,
    pub val: SymbolVal,
    pub function: Option<Value>,
    pub plist: FxHashMap<SymId, Value>,  // ← replace
    // internal bits
}
```

Operations:
- `put`: `sym.plist.insert(SymId, Value)` — O(1), loses duplicates + order.
- `get`: `sym.plist.get(&SymId)` — O(1).
- `symbol-plist`: synthesize cons from HashMap — fresh allocation every call.
- `setplist`: walk cons list, dedupe into HashMap.
- `trace_roots`: iterate `sym.plist.values()`.

### After

```rust
pub struct LispSymbol {
    pub name: NameId,
    pub flags: SymbolFlags,
    pub val: SymbolVal,
    pub function: Option<Value>,
    pub plist: Value,                    // ← Lisp cons list, NIL = empty
    // internal bits
}
```

Operations:
- `put`: `plist_put_eq(sym.plist, key, value)` (reuse from `buffer/overlay.rs`, move to a shared location or re-export). Walks chain, replaces in place or cons-prepends.
- `get`: `plist_get_eq(sym.plist, &key)`. Walks chain.
- `symbol-plist`: return `sym.plist` directly — no allocation, stable identity.
- `setplist`: `sym.plist = new_list;` — one pointer write.
- `trace_roots`: `roots.push(sym.plist);` — one push, GC walks the chain.

## Changes in detail

### 1. Move `plist_get_eq` / `plist_put_eq` to a shared location

Current home: `neovm-core/src/buffer/overlay.rs:509-560` (pub(crate)).

Two options:
- **A.** Move them to `neovm-core/src/emacs_core/value.rs` or a new `emacs_core/plist.rs` module. Make `pub(crate)`, re-export from wherever needed.
- **B.** Duplicate in `symbol.rs`. Small cost (~50 LOC), but creates drift risk.

**Choose A.** One source of truth.

New module: `neovm-core/src/emacs_core/plist.rs` with:

```rust
pub fn plist_get(plist: Value, prop: &Value) -> Option<Value>;
pub fn plist_put(plist: Value, prop: Value, value: Value) -> (Value, bool);
pub fn plist_member(plist: Value, prop: &Value) -> Value;  // returns tail or NIL
pub fn plist_walk(plist: Value, f: impl FnMut(Value, Value));
```

All use `eq_value` comparison (matches GNU `plist-get`/`put`). If `equal` variants are needed later, add `plist_get_equal`, etc.

### 2. `LispSymbol` struct field change (`symbol.rs`)

```rust
pub struct LispSymbol {
    // ... unchanged ...
-   pub plist: FxHashMap<SymId, Value>,
+   pub plist: Value,  // Lisp cons list. NIL = empty plist.
    // ...
}
```

All constructor/`Default` sites initialize `plist: Value::NIL`.

### 3. Symbol-plist accessors (`symbol.rs`)

Rewrite `plist_get_id`, `plist_put_id`, `plist_put_named`, `setplist`:

```rust
pub fn plist_get_id(&self, symbol: SymId, prop: SymId) -> Option<Value> {
    self.slot(symbol).and_then(|s| {
        plist::plist_get(s.plist, &Value::from_sym_id(prop))
    })
}

pub fn plist_put_id(&mut self, symbol: SymId, prop: SymId, value: Value) {
    let Some(sym) = self.slot_mut(symbol) else { return; };
    let (new_plist, _changed) = plist::plist_put(sym.plist, Value::from_sym_id(prop), value);
    sym.plist = new_plist;
}

pub fn setplist_id(&mut self, symbol: SymId, plist: Value) {
    if let Some(sym) = self.slot_mut(symbol) {
        sym.plist = plist;
    }
}
```

`plist_put_named(sym, name, value)` becomes `plist_put_id(sym, intern(name), value)`.

Delete any "iterate all entries" method (`for (k, v) in &sym.plist`) or rewrite to walk the cons list.

### 4. Builtins `(put)` / `(get)` / `(symbol-plist)` / `(setplist)` (`builtins/symbols.rs`)

The 2 call sites (`symbols.rs:203` iterating and `symbols.rs:252` setting) get migrated:

- `symbol-plist`: `obarray.slot(id).map(|s| s.plist).unwrap_or(Value::NIL)`.
- `setplist`: `obarray.setplist_id(id, new_plist)`.
- `put` / `get`: already route through the obarray accessor above, which now works on cons lists.

### 5. Pdump format change (`pdump/types.rs` + `pdump/convert.rs`)

Current dump type for symbol plist at `pdump/types.rs:345`:

```rust
pub plist: Vec<(DumpSymId, DumpValue)>,
```

Replace with:

```rust
pub plist: DumpValue,  // serialized cons list (or NIL)
```

This matches how `LispString::plist` and overlay plists are already dumped (as `DumpValue`).

Update:
- `dump_symbol_data` at `pdump/convert.rs:2411` and nearby: serialize `sym.plist` as a `DumpValue`, not a vec-of-pairs.
- `load_symbol_data`: set `symbol.plist = decode_value(sd.plist)`.

Bump pdump format version 21 → 22.

### 6. `Obarray::trace_roots` (`symbol.rs`)

Change:

```rust
-for pval in sym.plist.values() {
-    roots.push(*pval);
-}
+roots.push(sym.plist);
```

GC now walks the cons chain automatically via its existing cons-tracer.

### 7. Remove `FxHashMap` import if unused

If `FxHashMap` is only used by the deleted `plist` field, remove the `use` line. If other fields still use it, leave it.

## Data flow

```
(put 'foo 'color 'red)
  └→ builtin_put(foo, color, red)
      └→ obarray.plist_put_id(foo, color, red)
          └→ plist::plist_put(sym.plist, (Symbol color), (Symbol red))
              └→ walk cons chain; found color → overwrite cdr
                 or not found → (color red . old-plist)
          └→ sym.plist = new_plist  // pointer write

(get 'foo 'color)
  └→ builtin_get(foo, color)
      └→ obarray.plist_get_id(foo, color)
          └→ plist::plist_get(sym.plist, (Symbol color))
              └→ walk cons chain; return first match

(symbol-plist 'foo)
  └→ builtin_symbol_plist(foo)
      └→ obarray.slot(foo).map(|s| s.plist).unwrap_or(NIL)
          (returns the stored cons pointer, not a copy)

(setplist 'foo '(color red size 10))
  └→ builtin_setplist(foo, list)
      └→ obarray.setplist_id(foo, list)
          └→ sym.plist = list  // one pointer write
```

## Testing

Regression tests in a new `neovm-core/src/emacs_core/symbol_plist_regression_test.rs`:

1. **`plist_insertion_order_preserved`** — `(put 'foo 'a 1)` `(put 'foo 'b 2)` `(put 'foo 'c 3)`, then `(symbol-plist 'foo)` returns `(a 1 b 2 c 3)` in that order.

2. **`plist_duplicate_keys_preserved_by_setplist`** — `(setplist 'foo '(a 1 a 2))`, `(symbol-plist 'foo)` returns `(a 1 a 2)`; `(plist-get (symbol-plist 'foo) 'a)` returns `1`.

3. **`symbol_plist_returns_eq_identical_pointer`** — `(let ((p (symbol-plist 'foo))) (eq p (symbol-plist 'foo)))` returns `t`.

4. **`destructive_plist_mutation_visible_via_symbol_plist`** — `(put 'foo 'a 1)`, bind the plist via `(let ((p (symbol-plist 'foo))) … (plist-put p 'b 2))`, then `(plist-get (symbol-plist 'foo) 'b)` returns `2`. (Tests shared mutation.)

5. **`setplist_accepts_non_symbol_keys`** — `(setplist 'foo '("key1" 1 "key2" 2))`; `(plist-get (symbol-plist 'foo) "key1")` via string-key `equal` match (if NeoMacs's `plist-get` supports string keys as GNU's does).

6. **`symbol_plist_survives_gc`** — set a plist containing a freshly-consed large structure, force GC, read back, confirm value preserved. Catches `trace_roots` migration correctness.

7. **`put_get_unchanged_semantics`** — existing `put`/`get` loop-over works identically; large sample of N put/get pairs with unique keys round-trips.

## Migration plan

Four phases, each compile-clean + test-green.

### Phase A — Extract `plist::plist_get`/`plist_put`/`plist_member` helpers to `emacs_core/plist.rs`

Move the bodies of `plist_get_eq` / `plist_put_eq` from `buffer/overlay.rs` to the new module. Export `pub(crate)`. Update `buffer/overlay.rs` call sites. No behavior change.

**Risk:** low. Pure code move.

### Phase B — Add regression tests (all pass on `FxHashMap` version EXCEPT the structural ones, documenting which fail today)

Write the 7 tests above. Tests 3, 4, 5 will FAIL on current code — that's expected. Tests 1, 2, 6, 7 should pass today. This establishes the gate.

**Risk:** low.

### Phase C — Flip `LispSymbol::plist` to `Value`, migrate internal accessors + builtins

The load-bearing change:
- Change field type.
- Rewrite `plist_get_id` / `plist_put_id` / `setplist_id` in `symbol.rs`.
- Migrate `Obarray::trace_roots` to push `sym.plist` directly.
- Update `builtins/symbols.rs` callers.
- Update `pdump/convert.rs` dump/load to serialize `sym.plist` as a `DumpValue`.

**Risk:** medium. The 3 tests that failed in Phase B must now pass.

### Phase D — Pdump format v21 → v22

Change `DumpSymbolData::plist` field type to `DumpValue`. Bump format version. Update `dump_symbol_data` / `load_symbol_data`. Old dumps invalidated (per project memory S105).

**Risk:** medium (bootstrap dump regeneration needed).

## Risks

1. **Performance regression for large plists.** HashMap is O(1), cons-list is O(n). For a symbol with 50+ plist entries, `get`/`put` slows proportionally. GNU accepts this; we should too. Mitigation: if any hot-path site is identified, consider caching. Benchmark first.

2. **Shared mutation aliasing bugs.** With a shared cons pointer, `(symbol-plist foo)` returned to Lisp can be mutated and the mutation propagates. This is GNU behavior — if Lisp code was relying on NeoMacs's prior defensive-copy semantics, it may break. The regression tests exercise this; any newly-failing test flags a real caller assumption.

3. **Pdump format bump invalidates existing dumps.** Same as prior phases — `cargo xtask` regenerates.

4. **`plist_put_eq` returns `(new_plist, changed)` tuple.** Callers that discard `changed` are fine; if any site uses it to skip a modification-tick bump, verify the new callers preserve the behavior.

## Success criteria

- `grep -rn 'plist: FxHashMap\|plist.insert\|plist.values\|plist.get(' neovm-core/src` returns zero hits outside `plist.rs`'s own cons-list traversal and any legitimate non-symbol plist HashMap (there should be none).
- All 7 regression tests pass.
- `LispSymbol` has exactly these fields: `name`, `flags`, `val`, `function`, `plist: Value`, plus internal bits. Matches GNU layout (modulo `name: NameId` vs `Lisp_Object name`, which is the next refactor).
- Full crate test sweep: no new failures vs. baseline (post-Phase-10-merge state).
