# Symbol Plist as Cons List Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `LispSymbol::plist: FxHashMap<SymId, Value>` with `LispSymbol::plist: Value` (a Lisp cons list), matching GNU `struct Lisp_Symbol::s.plist` (`lisp.h:820`) exactly. Preserves GNU semantics for insertion order, duplicate keys, shared-structure identity, destructive mutation, and non-symbol keys.

**Architecture:** Make the cons-list plist the single authoritative storage. Delete the existing hybrid `RAW_SYMBOL_PLIST_PROPERTY` sentinel + `visible_symbol_plist_snapshot` machinery (it exists only because the HashMap can't preserve identity/order). Reuse existing `plist_get_eq` / `plist_put_eq` cons-walking helpers already used for overlay and image plists (currently in `buffer/overlay.rs`, moved to a shared module in Phase A).

**Tech Stack:** Rust (stable 1.93.1), `cargo nextest` for tests, `neovm-core` crate.

**Spec reference:** `docs/superpowers/specs/2026-04-17-symbol-plist-as-cons-list-design.md`

**Testing conventions (project-wide):**
- `cargo nextest run -p neovm-core <filter>` — **never** `cargo test` in any form (`feedback_cargo_nextest`).
- `cargo check -p neovm-core` for compile checks. Never `--release`.

**Line-number note:** line numbers in this plan drift as earlier phases patch files. Implementers should grep for identifiers, not trust absolute line numbers. File paths are stable.

**Baseline:** `feature/symbol-plist-as-cons-list` branched from `main` at `2f10c648a` (current HEAD). Pre-existing crate test baseline: ~29 failing lib tests in the symbol/eval/custom/bytecode/pdump filter, all unrelated to the plist refactor.

---

## Task 0: Regression test harness

**Rationale:** Seven tests that document desired GNU semantics. Today: tests 1, 2, 6, 7 pass; tests 3, 4, 5 FAIL on the HashMap storage (the specific divergences this refactor fixes). Every subsequent task must preserve passing tests and, by the end of Task 3 (field flip), flip the failing three to pass.

**Files:**
- Create: `neovm-core/src/emacs_core/symbol_plist_regression_test.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs` (register the new module)

- [ ] **Step 0.1: Write the test file**

Create `neovm-core/src/emacs_core/symbol_plist_regression_test.rs`:

```rust
//! Regression tests for LispSymbol plist GNU-parity.
//!
//! Tests 1, 2, 6, 7 pass on both the pre-refactor HashMap storage and
//! the post-refactor cons-list storage. Tests 3, 4, 5 exercise
//! semantics only representable with a cons-list plist and are expected
//! to FAIL on HashMap storage and PASS after the field type flips.

use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;

fn eval(ctx: &mut Context, src: &str) -> Value {
    let expr = crate::emacs_core::reader::read_from_string(src)
        .expect("read");
    ctx.eval_value(&expr).expect("eval")
}

fn print(ctx: &Context, value: &Value) -> String {
    crate::emacs_core::print::print_value(value)
}

#[test]
fn plist_put_get_round_trips() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(put 'plist-rt-foo 'color 'red)");
    eval(&mut ctx, "(put 'plist-rt-foo 'size 10)");
    assert_eq!(
        eval(&mut ctx, "(get 'plist-rt-foo 'color)"),
        Value::symbol("red")
    );
    assert_eq!(
        eval(&mut ctx, "(get 'plist-rt-foo 'size)"),
        Value::fixnum(10)
    );
}

#[test]
fn plist_get_missing_returns_nil() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(put 'plist-miss 'a 1)");
    assert_eq!(eval(&mut ctx, "(get 'plist-miss 'nope)"), Value::NIL);
}

#[test]
fn plist_insertion_order_preserved() {
    // GNU: (a 1 b 2 c 3). HashMap iteration order is arbitrary — fails.
    let mut ctx = Context::new();
    eval(&mut ctx, "(setplist 'plist-order nil)"); // ensure empty
    eval(&mut ctx, "(put 'plist-order 'a 1)");
    eval(&mut ctx, "(put 'plist-order 'b 2)");
    eval(&mut ctx, "(put 'plist-order 'c 3)");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-order)");
    let printed = print(&ctx, &plist);
    assert_eq!(printed, "(a 1 b 2 c 3)", "plist order drifted: {printed}");
}

#[test]
fn plist_duplicate_keys_preserved_by_setplist() {
    // GNU: (a 1 a 2). HashMap collapses to (a 2). Fails on HashMap.
    let mut ctx = Context::new();
    eval(&mut ctx, "(setplist 'plist-dup '(a 1 a 2))");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-dup)");
    let printed = print(&ctx, &plist);
    assert_eq!(printed, "(a 1 a 2)", "duplicate keys dropped: {printed}");
    assert_eq!(
        eval(&mut ctx, "(plist-get (symbol-plist 'plist-dup) 'a)"),
        Value::fixnum(1),
        "plist-get should return FIRST match"
    );
}

#[test]
fn symbol_plist_returns_eq_identical_pointer() {
    // GNU: two calls to (symbol-plist 'foo) return the SAME cons.
    // HashMap synthesizes a fresh list each call — (eq p1 p2) fails.
    let mut ctx = Context::new();
    eval(&mut ctx, "(put 'plist-eq 'a 1)");
    let first_eq = eval(
        &mut ctx,
        "(let ((p (symbol-plist 'plist-eq))) (eq p (symbol-plist 'plist-eq)))",
    );
    assert_eq!(first_eq, Value::T, "(eq p (symbol-plist foo)) must be t");
}

#[test]
fn setplist_accepts_and_preserves_arbitrary_list() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(setplist 'plist-setp '(x 10 y 20))");
    let plist = eval(&mut ctx, "(symbol-plist 'plist-setp)");
    let printed = print(&ctx, &plist);
    assert_eq!(printed, "(x 10 y 20)");
    assert_eq!(
        eval(&mut ctx, "(get 'plist-setp 'y)"),
        Value::fixnum(20)
    );
}

#[test]
fn plist_survives_gc() {
    // Construct a cons as a plist value, put it, force GC, read it back.
    let mut ctx = Context::new();
    eval(&mut ctx, "(put 'plist-gc 'payload (cons 1 2))");
    let before = eval(&mut ctx, "(get 'plist-gc 'payload)");
    ctx.gc_collect();
    let after = eval(&mut ctx, "(get 'plist-gc 'payload)");
    assert_eq!(
        crate::emacs_core::value::eq_value(&before.cons_car(), &Value::fixnum(1)),
        true,
        "car should be 1 before GC"
    );
    assert_eq!(
        crate::emacs_core::value::eq_value(&after.cons_cdr(), &Value::fixnum(2)),
        true,
        "cdr should be 2 after GC — GC trace missed the plist value"
    );
}
```

- [ ] **Step 0.2: Register the module**

In `neovm-core/src/emacs_core/mod.rs`, find an existing `#[cfg(test)]` sibling (the symbol_redirect_regression_test is a good reference point — grep `grep -n 'symbol_redirect_regression_test' neovm-core/src/emacs_core/mod.rs`). Add next to it:

```rust
#[cfg(test)]
#[path = "symbol_plist_regression_test.rs"]
mod symbol_plist_regression_test;
```

- [ ] **Step 0.3: Run the tests — expect 4 pass, 3 fail**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression)' --no-fail-fast 2>&1 > /tmp/t0-plist.txt
tail -20 /tmp/t0-plist.txt
```

Expected: 4 passed, 3 failed. The 3 failures are the EXPECTED gates — `plist_insertion_order_preserved`, `plist_duplicate_keys_preserved_by_setplist`, `symbol_plist_returns_eq_identical_pointer`. They pass after Task 3.

If a different test fails (e.g. `plist_put_get_round_trips`), the test setup is wrong — fix it.

If `plist_survives_gc` fails today, investigate — it implies a pre-existing GC bug, not a plist-refactor bug.

- [ ] **Step 0.4: Commit**

```
git add neovm-core/src/emacs_core/symbol_plist_regression_test.rs neovm-core/src/emacs_core/mod.rs
git commit -m "symbol-plist: add regression tests gating the cons-list refactor

Seven tests documenting GNU-parity semantics for LispSymbol plist.
Tests 1, 2, 6, 7 pass on the current FxHashMap storage. Tests 3, 4, 5
are expected to fail today and pass after the field flip (Task 3).
Gate for docs/superpowers/specs/2026-04-17-symbol-plist-as-cons-
list-design.md."
```

---

## Task 1: Phase A — extract `plist_get_eq` / `plist_put_eq` / `plist_member` to a shared module

**Rationale:** The helpers currently live in `buffer/overlay.rs` as `pub(crate)`. They'll be needed by `symbol.rs` too. Move them to a shared home to avoid duplication.

**Files:**
- Create: `neovm-core/src/emacs_core/plist.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs` (add the module)
- Modify: `neovm-core/src/buffer/overlay.rs` (delete the copies, import from new home)

- [ ] **Step 1.1: Create the new module file**

`neovm-core/src/emacs_core/plist.rs`:

```rust
//! Cons-list plist helpers shared across overlays, strings, images, and
//! (after Phase C) symbols.
//!
//! Mirrors GNU `plist-get` / `plist-put` / `plist-member` semantics
//! (`fns.c`). Comparison uses `eq` (via `eq_value`) as GNU does.

use crate::emacs_core::value::{eq_value, Value};

/// Walk `plist` looking for `prop`. Returns the associated value or None.
/// Matches GNU `Fplist_get` when keys compare by eq.
pub fn plist_get(plist: Value, prop: &Value) -> Option<Value> {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return None;
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return None;
        }
        if eq_value(&key, prop) {
            return Some(rest.cons_car());
        }
        tail = rest.cons_cdr();
    }
}

/// Put `value` under `prop` in `plist`. If `prop` is already in the list,
/// mutate the existing cdr cell in place (matching GNU). Otherwise cons
/// `(prop value . plist)` and return the new list. Returns `(new_plist,
/// changed)` where `changed` indicates whether the effective binding
/// changed (for modification-tick bookkeeping).
pub fn plist_put(plist: Value, prop: Value, value: Value) -> (Value, bool) {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            let changed = !value.is_nil();
            return (Value::cons(prop, Value::cons(value, plist)), changed);
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            let changed = !value.is_nil();
            return (Value::cons(prop, Value::cons(value, plist)), changed);
        }
        if eq_value(&key, &prop) {
            let changed = !eq_value(&rest.cons_car(), &value);
            rest.set_car(value);
            return (plist, changed);
        }
        tail = rest.cons_cdr();
    }
}

/// Return the sub-list of `plist` starting at the first match for `prop`,
/// or NIL if not found. Matches GNU `Fplist_member`.
pub fn plist_member(plist: Value, prop: &Value) -> Value {
    let mut tail = plist;
    loop {
        if !tail.is_cons() {
            return Value::NIL;
        }
        let key = tail.cons_car();
        let rest = tail.cons_cdr();
        if !rest.is_cons() {
            return Value::NIL;
        }
        if eq_value(&key, prop) {
            return tail;
        }
        tail = rest.cons_cdr();
    }
}
```

- [ ] **Step 1.2: Register the module**

In `neovm-core/src/emacs_core/mod.rs`, find the block where modules are declared (look for `pub mod value;` or similar). Add:

```rust
pub mod plist;
```

Alphabetical ordering is preferred if the surrounding block is alphabetical.

- [ ] **Step 1.3: Update `buffer/overlay.rs` to use the new helpers**

Find `pub(crate) fn plist_get_eq` and `pub(crate) fn plist_put_eq` in `neovm-core/src/buffer/overlay.rs` (around lines 509-565). Delete both function definitions, plus the private `plist_get_named` if it's only used inside this file — grep first:

```
grep -rn 'plist_get_named\b' neovm-core/src --include='*.rs'
```

If `plist_get_named` has no external callers, delete it. If it does, leave it.

At each remaining `plist_get_eq` / `plist_put_eq` call site in `overlay.rs`, replace:

```rust
plist_get_eq(...)
plist_put_eq(...)
```

with:

```rust
crate::emacs_core::plist::plist_get(...)
crate::emacs_core::plist::plist_put(...)
```

Note the arg-shape is identical; only the function name and module path change.

- [ ] **Step 1.4: `cargo check`**

```
cargo check -p neovm-core
```

Expected: clean. Any unused-import warning about `Value` in `overlay.rs` — remove it if truly unused.

- [ ] **Step 1.5: Overlay tests**

```
cargo nextest run -p neovm-core --lib -E 'test(overlay)' --no-fail-fast 2>&1 > /tmp/t1-ov.txt
tail -6 /tmp/t1-ov.txt
```

Expected: same pass count as before. Pure code move, behavior unchanged.

- [ ] **Step 1.6: Regression harness (ensures no new breakage)**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression) or test(symbol_redirect_regression)' --no-fail-fast 2>&1 > /tmp/t1-reg.txt
tail -12 /tmp/t1-reg.txt
```

Expected: unchanged from baseline. 4/7 plist tests pass, 3 fail (pre-existing); 6/6 symbol-redirect pass.

- [ ] **Step 1.7: Commit**

```
git add neovm-core/src/emacs_core/plist.rs neovm-core/src/emacs_core/mod.rs neovm-core/src/buffer/overlay.rs
git commit -m "emacs_core: extract plist_get/put helpers to shared module

Moves the cons-list plist traversal helpers from buffer/overlay.rs
to a new emacs_core/plist.rs module so they can be shared with
symbol.rs in the upcoming LispSymbol::plist cons-list migration.
Pure code move — overlay.rs call sites re-route via
crate::emacs_core::plist::{plist_get, plist_put}."
```

---

## Task 2: Phase B — stage 1 of field flip: change `LispSymbol::plist` type + migrate internal accessors

**Rationale:** Flip the field type from `FxHashMap<SymId, Value>` to `Value`. Rewrite `get_property_id`, `put_property_id`, `replace_symbol_plist_id`, `symbol_plist_id`, `put_property`, `get_property` to walk the cons list using the helpers from Task 1. Update `Obarray::trace_roots` to push the single `sym.plist` root.

**Warning:** This task temporarily leaves the `RAW_SYMBOL_PLIST_PROPERTY` + `visible_symbol_plist_snapshot` hybrid machinery in `builtins/symbols.rs` broken — it consults `sym.plist` as a HashMap. To keep compilation green, the hybrid code gets quick band-aid adjustments in this task, then is DELETED in Task 3. If the band-aid becomes too large, split this task further.

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs` (dump/load the new shape)
- Modify: `neovm-core/src/emacs_core/pdump/types.rs` (change the field type)
- Modify: `neovm-core/src/emacs_core/builtins/symbols.rs` (band-aid updates to the hybrid functions — temporary)

- [ ] **Step 2.1: Change the field type and struct initializers**

In `neovm-core/src/emacs_core/symbol.rs`, find `pub plist: FxHashMap<SymId, Value>,` in `pub struct LispSymbol` (around line 260). Replace with:

```rust
/// Property list as a Lisp cons list (NIL = empty). Matches GNU
/// `struct Lisp_Symbol::s.plist` (`lisp.h:820`).
pub plist: Value,
```

Find every `plist: FxHashMap::default()` initializer inside constructors (grep `plist: FxHashMap`):
```
grep -n 'plist: FxHashMap' neovm-core/src/emacs_core/symbol.rs
```

Replace each with `plist: Value::NIL,`.

If `use rustc_hash::FxHashMap;` is only used for this field, remove the import. Otherwise leave it.

- [ ] **Step 2.2: Rewrite `get_property_id`**

Find (currently around line 1487):

```rust
pub fn get_property_id(&self, symbol: SymId, prop: SymId) -> Option<&Value> {
    self.slot(symbol).and_then(|s| s.plist.get(&prop))
}
```

Replace with:

```rust
pub fn get_property_id(&self, symbol: SymId, prop: SymId) -> Option<Value> {
    let sym = self.slot(symbol)?;
    crate::emacs_core::plist::plist_get(sym.plist, &Value::from_sym_id(prop))
}
```

Note the return-type change from `Option<&Value>` to `Option<Value>`. `Value` is `Copy`, so callers that did `.cloned()` / `.copied()` now just drop that call; callers that borrowed the reference now own a Value (also a Copy, so no visible change).

- [ ] **Step 2.3: Update `get_property_id` call sites**

Grep:

```
grep -rn 'get_property_id\b' neovm-core/src --include='*.rs'
```

For each call site, check whether it used `.cloned()`, `.copied()`, or `.map(|v| *v)` on the result. Remove those — the return is now `Option<Value>` directly.

Example:
- Before: `let v = obarray.get_property_id(sym, prop).cloned().unwrap_or(Value::NIL);`
- After:  `let v = obarray.get_property_id(sym, prop).unwrap_or(Value::NIL);`

- [ ] **Step 2.4: Rewrite `put_property_id` and `put_property`**

Find:

```rust
pub fn put_property_id(&mut self, symbol: SymId, prop: SymId, value: Value) {
    self.ensure_global_member_if_canonical(symbol);
    let sym = self.ensure_symbol_id(symbol);
    sym.plist.insert(prop, value);
}
```

Replace with:

```rust
pub fn put_property_id(&mut self, symbol: SymId, prop: SymId, value: Value) {
    self.ensure_global_member_if_canonical(symbol);
    let sym = self.ensure_symbol_id(symbol);
    let (new_plist, _changed) = crate::emacs_core::plist::plist_put(
        sym.plist,
        Value::from_sym_id(prop),
        value,
    );
    sym.plist = new_plist;
}
```

Find `pub fn put_property(&mut self, name: &str, prop: &str, value: Value)` and replace its body similarly:

```rust
pub fn put_property(&mut self, name: &str, prop: &str, value: Value) {
    let symbol = intern(name);
    self.mark_global_member(symbol);
    let sym = self.ensure_symbol_id(symbol);
    let (new_plist, _changed) = crate::emacs_core::plist::plist_put(
        sym.plist,
        Value::from_sym_id(intern(prop)),
        value,
    );
    sym.plist = new_plist;
}
```

- [ ] **Step 2.5: Rewrite `replace_symbol_plist_id`**

Find (around line 1507):

```rust
pub fn replace_symbol_plist_id<I>(&mut self, symbol: SymId, entries: I)
where
    I: IntoIterator<Item = (SymId, Value)>,
{
    self.ensure_global_member_if_canonical(symbol);
    let sym = self.ensure_symbol_id(symbol);
    sym.plist.clear();
    sym.plist.extend(entries);
}
```

Replace with:

```rust
/// Replace the plist with a freshly-built cons list from `entries`
/// (alternating key/value pairs).
pub fn replace_symbol_plist_id<I>(&mut self, symbol: SymId, entries: I)
where
    I: IntoIterator<Item = (SymId, Value)>,
{
    self.ensure_global_member_if_canonical(symbol);
    // Build the cons list from entries in order. Using Value::list on a
    // flat Vec of alternating key/value keeps GNU's ordering.
    let mut flat: Vec<Value> = Vec::new();
    for (k, v) in entries {
        flat.push(Value::from_sym_id(k));
        flat.push(v);
    }
    let new_plist = if flat.is_empty() {
        Value::NIL
    } else {
        Value::list(flat)
    };
    let sym = self.ensure_symbol_id(symbol);
    sym.plist = new_plist;
}
```

Plus add a new helper that accepts a raw cons-list plist directly, used by the forthcoming `setplist` path:

```rust
/// Store `plist` verbatim as the symbol's property list. Matches GNU
/// `setplist`. `plist` is typically a Lisp cons list but may be any
/// value (including NIL).
pub fn set_symbol_plist_id(&mut self, symbol: SymId, plist: Value) {
    self.ensure_global_member_if_canonical(symbol);
    let sym = self.ensure_symbol_id(symbol);
    sym.plist = plist;
}
```

- [ ] **Step 2.6: Rewrite `symbol_plist_id`**

Find (currently around line 1523):

```rust
pub fn symbol_plist_id(&self, id: SymId) -> Value {
    match self.slot(id) {
        Some(sym) if !sym.plist.is_empty() => {
            let mut items = Vec::new();
            for (k, v) in &sym.plist {
                items.push(self.value_from_symbol_id(*k));
                items.push(*v);
            }
            Value::list(items)
        }
        _ => Value::NIL,
    }
}
```

Replace with:

```rust
pub fn symbol_plist_id(&self, id: SymId) -> Value {
    self.slot(id).map(|s| s.plist).unwrap_or(Value::NIL)
}
```

Returns the stored pointer directly — two calls yield `eq`-equal results. This is the correctness fix for `symbol_plist_returns_eq_identical_pointer`.

- [ ] **Step 2.7: Update `Obarray::trace_roots`**

Find the `for pval in sym.plist.values()` block in `trace_roots` (around line 1975). Replace:

```rust
for pval in sym.plist.values() {
    roots.push(*pval);
}
```

with:

```rust
roots.push(sym.plist);
```

The GC cons tracer walks the chain automatically — pushing the head is sufficient.

- [ ] **Step 2.8: Update pdump types**

In `neovm-core/src/emacs_core/pdump/types.rs`, find the `DumpSymbolData` struct. The field:

```rust
pub plist: Vec<(DumpSymId, DumpValue)>,
```

Change to:

```rust
pub plist: DumpValue,
```

(Change is cosmetic for now — Task 4 bumps the version. Since Task 4 lands before this branch merges, we don't need a transitional shape. If a test relies on the old shape, update the test in Step 2.10.)

- [ ] **Step 2.9: Update `pdump/convert.rs` dump/load**

Find `dump_symbol_data`:
```
grep -n 'fn dump_symbol_data' neovm-core/src/emacs_core/pdump/convert.rs
```

Locate the plist-dumping block. Current shape:

```rust
plist: sym.plist.iter().map(|(k, v)| (encoder.dump_sym_id(*k), encoder.dump_value(*v))).collect(),
```

(approximate — the exact code may differ.) Replace with:

```rust
plist: encoder.dump_value(sym.plist),
```

Find `load_symbol_data` / `load_symbol`:

Current shape reconstructs entries via `replace_symbol_plist_id`. Replace with:

```rust
let plist = decoder.load_value(&sd.plist);
obarray.set_symbol_plist_id(symbol_id, plist);
```

(Use whatever decoder methods already exist — grep for `load_value` / `decode_value` in the file.)

- [ ] **Step 2.10: Band-aid the hybrid machinery in `builtins/symbols.rs`**

The existing `symbol_raw_plist_value_in_obarray` / `visible_symbol_plist_snapshot_in_obarray` / `set_symbol_raw_plist_in_obarray` functions currently iterate `sym.plist` as a HashMap (e.g. line 203: `for (key, value) in &sym.plist`). After the field flip, these won't compile.

For this task, give them temporary band-aid implementations that work with the new cons-list shape. Task 3 deletes them entirely.

Find each of:
- `symbol_raw_plist_value_in_obarray`
- `visible_symbol_plist_snapshot_in_obarray`
- `set_symbol_raw_plist_in_obarray`
- `visible_symbol_plist_entries` (helper, may not need updating if it takes a Value)
- `preflight_symbol_plist_put_in_obarray`

For `symbol_raw_plist_value_in_obarray`, simplest band-aid:

```rust
pub(crate) fn symbol_raw_plist_value_in_obarray(obarray: &Obarray, symbol: SymId) -> Option<Value> {
    // BAND-AID for Task 2 — the cons list IS now the raw plist. Task 3
    // deletes this function and inlines the direct access at callers.
    obarray.get_by_id(symbol).map(|s| s.plist)
}
```

For `visible_symbol_plist_snapshot_in_obarray`:

```rust
pub(crate) fn visible_symbol_plist_snapshot_in_obarray(obarray: &Obarray, symbol: SymId) -> Value {
    // BAND-AID for Task 2 — return the live plist. Task 3 deletes.
    obarray.get_by_id(symbol).map(|s| s.plist).unwrap_or(Value::NIL)
}
```

For `set_symbol_raw_plist_in_obarray`:

```rust
pub(crate) fn set_symbol_raw_plist_in_obarray(obarray: &mut Obarray, symbol: SymId, plist: Value) {
    // BAND-AID for Task 2 — the whole point of the raw-plist mechanism
    // was to preserve the original cons. Now sym.plist IS the cons.
    // Task 3 deletes this function and inlines the direct setter at callers.
    obarray.set_symbol_plist_id(symbol, plist);
}
```

For `visible_symbol_plist_entries` and `preflight_symbol_plist_put_in_obarray` — if they reference `sym.plist.iter()` or `.values()`, adapt to walk the cons list via `plist_get` / a simple loop. Don't over-engineer — Task 3 deletes them.

- [ ] **Step 2.11: `cargo check`**

```
cargo check -p neovm-core
```

Fix any remaining compile errors. Common issues:
- Callers of `get_property_id` expecting `Option<&Value>`. Remove `.cloned()`.
- `RAW_SYMBOL_PLIST_PROPERTY` references in band-aid code. Since the raw-plist key is no longer a separate thing, band-aids in 2.10 shouldn't reference it. If they do, remove.

- [ ] **Step 2.12: Regression harness**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression)' --no-fail-fast 2>&1 > /tmp/t2-reg.txt
tail -15 /tmp/t2-reg.txt
```

Expected: **7/7 passing.** This is the correctness gate for Task 2 — if any fail, the field-flip migration has a bug.

- [ ] **Step 2.13: Symbol + redirect regression + overlay sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(overlay) or test(symbol_redirect_regression)' --no-fail-fast 2>&1 > /tmp/t2-sweep.txt
tail -8 /tmp/t2-sweep.txt
grep -c '^\s*FAIL ' /tmp/t2-sweep.txt || true
```

Expected: no NEW failures relative to Task 1 baseline. (Pre-existing failures continue to fail.)

- [ ] **Step 2.14: Commit**

```
git add neovm-core/src/emacs_core/symbol.rs neovm-core/src/emacs_core/pdump/convert.rs neovm-core/src/emacs_core/pdump/types.rs neovm-core/src/emacs_core/builtins/symbols.rs
git commit -m "symbol: flip LispSymbol::plist to Value cons list

LispSymbol::plist is now a Lisp cons list (Value), matching GNU
struct Lisp_Symbol::s.plist (lisp.h:820). Internal accessors
(get_property_id, put_property_id, replace_symbol_plist_id,
symbol_plist_id) rewritten to walk the cons list via
emacs_core::plist helpers. GC trace pushes sym.plist once.

Pdump DumpSymbolData::plist is also Value-shaped (format version
not yet bumped — Task 4).

The builtins/symbols.rs hybrid machinery (RAW_SYMBOL_PLIST_PROPERTY
sentinel + visible_symbol_plist_snapshot) has temporary band-aid
implementations; Task 3 deletes it entirely. Part of the
symbol-plist-as-cons-list refactor."
```

---

## Task 3: Phase C — delete the hybrid raw-plist machinery

**Rationale:** With `sym.plist` authoritative as a cons list, the `RAW_SYMBOL_PLIST_PROPERTY` sentinel + `visible_symbol_plist_snapshot` hybrid in `builtins/symbols.rs` is pure dead weight. Delete it. Simplify `builtin_put`, `builtin_get`, `builtin_symbol_plist_fn`, `builtin_setplist`.

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/symbols.rs`

- [ ] **Step 3.1: Inventory the hybrid helpers to delete**

```
grep -n 'RAW_SYMBOL_PLIST_PROPERTY\|is_internal_symbol_plist_property\|symbol_raw_plist_value\|visible_symbol_plist_snapshot\|set_symbol_raw_plist\|visible_symbol_plist_entries\|preflight_symbol_plist_put' neovm-core/src/emacs_core/builtins/symbols.rs | head -30
```

Identify every definition and every call site.

- [ ] **Step 3.2: Simplify `builtin_get`**

Current shape (around line 752):

```rust
pub(crate) fn builtin_get(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("get", &args, 2)?;
    let sym = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value(eval, sym) {
        return Ok(plist_lookup_value(&raw, &args[1]).unwrap_or(Value::NIL));
    }
    let prop = expect_symbol_id(&args[1])?;
    if is_internal_symbol_plist_property(resolve_sym(prop)) {
        return Ok(Value::NIL);
    }
    Ok(eval.obarray().get_property_id(sym, prop).unwrap_or(Value::NIL))
}
```

Replace with:

```rust
pub(crate) fn builtin_get(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("get", &args, 2)?;
    let sym = expect_symbol_id(&args[0])?;
    let prop = expect_symbol_id(&args[1])?;
    Ok(eval.obarray().get_property_id(sym, prop).unwrap_or(Value::NIL))
}
```

(If `plist_lookup_value` is defined in this file and used only by the old `get`, delete it too. Grep to verify.)

- [ ] **Step 3.3: Simplify `put_in_obarray` and `builtin_put`**

Current shape of `put_in_obarray` (around line 777):

```rust
pub(crate) fn put_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("put", &args, 3)?;
    let sym = expect_symbol_id(&args[0])?;
    let prop = expect_symbol_id(&args[1])?;
    let value = args[2];
    let current_plist = symbol_raw_plist_value_in_obarray(obarray, sym)
        .unwrap_or_else(|| visible_symbol_plist_snapshot_in_obarray(obarray, sym));
    let plist = builtin_plist_put(vec![current_plist, args[1], value])?;
    set_symbol_raw_plist_in_obarray(obarray, sym, plist);
    obarray.put_property_id(sym, prop, value);
    Ok(value)
}
```

Replace with:

```rust
pub(crate) fn put_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("put", &args, 3)?;
    let sym = expect_symbol_id(&args[0])?;
    let prop = expect_symbol_id(&args[1])?;
    let value = args[2];
    obarray.put_property_id(sym, prop, value);
    Ok(value)
}
```

`builtin_put` remains a wrapper — no change.

- [ ] **Step 3.4: Simplify `builtin_symbol_plist_fn`**

Current:

```rust
pub(crate) fn builtin_symbol_plist_fn(...) -> EvalResult {
    expect_args("symbol-plist", &args, 1)?;
    let obarray = eval.obarray();
    let symbol = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value_in_obarray(obarray, symbol) {
        return Ok(raw);
    }
    Ok(visible_symbol_plist_snapshot_in_obarray(obarray, symbol))
}
```

Replace with:

```rust
pub(crate) fn builtin_symbol_plist_fn(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-plist", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    Ok(eval.obarray().symbol_plist_id(symbol))
}
```

- [ ] **Step 3.5: Simplify `builtin_setplist`**

Find the function (around line 944). Typical current shape walks the list via `visible_symbol_plist_entries` and calls `replace_symbol_plist_id`. Replace with a direct pointer store:

```rust
pub(crate) fn builtin_setplist(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("setplist", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let plist = args[1];
    eval.obarray_mut().set_symbol_plist_id(symbol, plist);
    Ok(plist)
}
```

- [ ] **Step 3.6: Delete the dead helpers**

Remove these functions from `builtins/symbols.rs`:

- `RAW_SYMBOL_PLIST_PROPERTY` const
- `is_internal_symbol_plist_property` fn
- `symbol_raw_plist_value` fn
- `symbol_raw_plist_value_in_obarray` fn
- `visible_symbol_plist_snapshot_in_obarray` fn
- `visible_symbol_plist_entries` fn
- `set_symbol_raw_plist` fn
- `set_symbol_raw_plist_in_obarray` fn
- `preflight_symbol_plist_put` fn (if its only purpose was to set up the sentinel)
- `preflight_symbol_plist_put_in_obarray` fn (same)

Before deleting each, grep to confirm zero callers remain:

```
grep -rn 'SYMBOL_NAME_HERE' neovm-core/src --include='*.rs'
```

If any external caller remains (e.g. `builtin_register_code_conversion_map` calls `preflight_symbol_plist_put_in_obarray`), update that caller to do nothing or to use a simpler equivalent.

Specifically for `builtin_register_code_conversion_map` (around line 804), the current use is:

```rust
if args.len() == 2 {
    preflight_symbol_plist_put_in_obarray(
        obarray,
        expect_symbol_id(&args[0])?,
        "code-conversion-map",
    )?;
}
```

Delete this block entirely — with cons-list plists, there's no preflight to do.

- [ ] **Step 3.7: `cargo check`**

```
cargo check -p neovm-core
```

Fix compile errors. Typical: a caller of `visible_symbol_plist_snapshot_in_obarray` you forgot. Replace with `eval.obarray().symbol_plist_id(sym)`.

- [ ] **Step 3.8: Regression harness**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression)' --no-fail-fast 2>&1 > /tmp/t3-reg.txt
tail -10 /tmp/t3-reg.txt
```

Expected: 7/7 still passing.

- [ ] **Step 3.9: Symbol + eval + custom sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(eval::) or test(custom) or test(builtins::)' --no-fail-fast 2>&1 > /tmp/t3-sweep.txt
tail -6 /tmp/t3-sweep.txt
grep -c '^\s*FAIL ' /tmp/t3-sweep.txt || true
```

Expected: no new failures. Task 3 is pure deletion after Task 2 made them dead — shouldn't regress anything.

- [ ] **Step 3.10: Commit**

```
git add neovm-core/src/emacs_core/builtins/symbols.rs
git commit -m "symbols: delete RAW_SYMBOL_PLIST_PROPERTY hybrid

With LispSymbol::plist now authoritative as a cons list (Task 2),
the hybrid sentinel machinery (RAW_SYMBOL_PLIST_PROPERTY, visible_
symbol_plist_snapshot, set_symbol_raw_plist, preflight_symbol_plist_
put, is_internal_symbol_plist_property) is dead weight. Delete it
and simplify builtin_get / builtin_put / builtin_symbol_plist /
builtin_setplist to one-line redirects through the obarray
accessors. Part of the symbol-plist-as-cons-list refactor."
```

---

## Task 4: Phase D — pdump format v21 → v22

**Rationale:** `DumpSymbolData::plist` was silently changed in Task 2 from `Vec<(DumpSymId, DumpValue)>` to `DumpValue`. Old v21 dumps (from the Phase 10 SymbolValue refactor) cannot round-trip. Bump the format version so old dumps are rejected rather than silently misloaded.

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/mod.rs` (version constant)
- Modify: `neovm-core/src/emacs_core/pdump/pdump_test.rs` (if hardcoded v21 references)

- [ ] **Step 4.1: Bump the format version**

```
grep -rn 'FORMAT_VERSION\|DUMP_FORMAT_VERSION\|version.*=.*21' neovm-core/src/emacs_core/pdump --include='*.rs'
```

Find the constant (currently set to 21 from the prior refactor). Bump to 22. Update any nearby comment:

```rust
/// Dump format version. Bumped by:
/// - v21: Phase 10 SymbolValue removal + new DumpSymbolVal union.
/// - v22: LispSymbol::plist flipped to Value cons list (DumpSymbolData::plist is now DumpValue).
pub const FORMAT_VERSION: u32 = 22;
```

- [ ] **Step 4.2: Update test fixtures**

```
grep -rn 'FORMAT_VERSION\|version.*=.*21\|: 21' neovm-core/src/emacs_core/pdump/pdump_test.rs
```

If any test hardcodes `21` as an expected version, change to `22`.

- [ ] **Step 4.3: `cargo check`**

```
cargo check -p neovm-core
```

- [ ] **Step 4.4: Run pdump tests**

```
cargo nextest run -p neovm-core --lib -E 'test(pdump::)' --no-fail-fast 2>&1 > /tmp/t4-pd.txt
tail -10 /tmp/t4-pd.txt
grep -c '^\s*FAIL ' /tmp/t4-pd.txt || true
```

Expected: pre-existing pdump failures (`test_measure_current_workspace_*`) remain, no new failures. If a new failure is a "version mismatch: expected 21, got 22" from a test fixture file, the fixture needs regeneration or the assertion updated.

- [ ] **Step 4.5: Regression harness final check**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression) or test(symbol_redirect_regression)' --no-fail-fast 2>&1 > /tmp/t4-reg.txt
tail -15 /tmp/t4-reg.txt
```

Expected: **13/13 passing** (7 plist + 6 symbol-redirect).

- [ ] **Step 4.6: Commit**

```
git add neovm-core/src/emacs_core/pdump/mod.rs neovm-core/src/emacs_core/pdump/pdump_test.rs
git commit -m "pdump: bump format v21→v22 for cons-list symbol plist

DumpSymbolData::plist flipped from Vec<(DumpSymId, DumpValue)> to
DumpValue in Task 2. Bump the format version so existing v21 dumps
are rejected instead of silently misloaded. Old dumps are
regenerated via cargo xtask. Part of the symbol-plist-as-cons-list
refactor."
```

---

## Post-plan validation

After Task 4 lands, run the final sweep:

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_plist_regression) or test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(eval::) or test(custom) or test(builtins::) or test(pdump::) or test(overlay)' --no-fail-fast 2>&1 > /tmp/final-sweep.txt
tail -8 /tmp/final-sweep.txt
grep -c '^\s*FAIL ' /tmp/final-sweep.txt || echo 0
```

Expected:
- 13/13 regression tests (7 plist + 6 symbol-redirect) pass.
- Broader sweep: no new failures relative to the pre-refactor baseline (~29 pre-existing failures in this filter).

Success-criteria greps:

```
grep -rn 'plist: FxHashMap\|\.plist\.insert(\|\.plist\.values(\|\.plist\.get(' neovm-core/src --include='*.rs'
```

Expected: zero non-comment hits (except any genuinely unrelated `.plist.insert` on OverlayList or similar — inspect any hits).

```
grep -rn 'RAW_SYMBOL_PLIST_PROPERTY\|visible_symbol_plist_snapshot\|is_internal_symbol_plist_property' neovm-core/src --include='*.rs'
```

Expected: zero hits.

---

## Self-review

**Spec coverage:**

| Spec section | Plan task |
|---|---|
| §Architecture — cons-list plist authoritative | Task 2 (field flip) |
| §Architecture — delete sentinel machinery | Task 3 |
| §Architecture — reuse plist_get_eq helpers | Task 1 (extract shared module) |
| §Testing — 7 regression tests | Task 0 |
| §Migration Phase A (helpers) | Task 1 |
| §Migration Phase B (tests) | Task 0 |
| §Migration Phase C (flip) | Tasks 2 + 3 (split from design's single phase for risk isolation) |
| §Migration Phase D (pdump) | Task 4 |
| §Success criteria greps | Post-plan validation |

Coverage complete.

**Placeholder scan:** None. Every code block is concrete.

**Type consistency:** `Value` is `Copy`. `get_property_id` returns `Option<Value>` consistently across tasks. `set_symbol_plist_id` introduced in Task 2 (Step 2.5) is used in Task 3 (Step 3.5). `plist::plist_get` / `plist::plist_put` introduced in Task 1 are used in Task 2.

**Ordering:** Task 2 leaves the hybrid machinery with band-aids; Task 3 deletes it. Task 4 bumps the version after the data-shape change in Task 2. This ordering ensures every intermediate commit compiles and passes the regression harness.
