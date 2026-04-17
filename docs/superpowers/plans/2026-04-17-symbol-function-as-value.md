# Symbol `function` Field as `Value` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `LispSymbol::function: Option<Value>` with `LispSymbol::function: Value`, using `Value::NIL` as the unbound sentinel — matching GNU `struct Lisp_Symbol::s.function` exactly.

**Architecture:** Three tasks: (T0) regression harness that passes today and continues passing through the refactor, (T1) single-commit field flip migrating every reader/writer/GC-trace, (T2) pdump format bump v22→v23 for the `DumpSymbolData::function: Option<DumpValue>` → `DumpValue` shape change.

**Tech Stack:** Rust (stable 1.93.1), `cargo nextest`, `neovm-core` crate.

**Spec reference:** `docs/superpowers/specs/2026-04-17-symbol-function-as-value-design.md`

**Testing conventions:** `cargo nextest run -p neovm-core <filter>` — **never** `cargo test`. `cargo check -p neovm-core` for compile checks; never `--release`.

**Baseline:** `feature/symbol-function-as-value` branched from `main` at `bf74ee815`. The prior follow-up (plist-as-cons) is already merged.

---

## Task 0: Regression test harness

**Files:**
- Create: `neovm-core/src/emacs_core/symbol_function_regression_test.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs` (register the module)

- [ ] **Step 0.1: Write the test file**

Create `neovm-core/src/emacs_core/symbol_function_regression_test.rs`:

```rust
//! Regression tests for LispSymbol::function GNU-parity.
//!
//! All 5 tests pass today (the semantic is already GNU-equivalent —
//! `Qnil` and `None` both mean "unbound function"). They must continue
//! to pass after T1 flips the field type to a bare `Value` with
//! `Value::NIL` as the unbound sentinel.

use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;

fn eval(ctx: &mut Context, src: &str) -> Value {
    let expr = crate::emacs_core::reader::read_from_string(src).expect("read");
    ctx.eval_value(&expr).expect("eval")
}

#[test]
fn fboundp_unbound_returns_nil() {
    let mut ctx = Context::new();
    assert_eq!(eval(&mut ctx, "(fboundp 'fn-unbound-a)"), Value::NIL);
}

#[test]
fn fset_then_fboundp_returns_t() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(fset 'fn-foo 'bar)");
    assert_eq!(eval(&mut ctx, "(fboundp 'fn-foo)"), Value::T);
}

#[test]
fn fmakunbound_resets_fboundp() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(fset 'fn-mu 'bar)");
    assert_eq!(eval(&mut ctx, "(fboundp 'fn-mu)"), Value::T);
    eval(&mut ctx, "(fmakunbound 'fn-mu)");
    assert_eq!(eval(&mut ctx, "(fboundp 'fn-mu)"), Value::NIL);
}

#[test]
fn fset_to_nil_is_unbound() {
    // GNU semantic: Qnil in the function slot means unbound.
    // (fset 'foo nil) and (fboundp 'foo) should return nil.
    let mut ctx = Context::new();
    eval(&mut ctx, "(fset 'fn-fn-nil nil)");
    assert_eq!(eval(&mut ctx, "(fboundp 'fn-fn-nil)"), Value::NIL);
}

#[test]
fn symbol_function_survives_gc() {
    let mut ctx = Context::new();
    // Define an anonymous lambda in the function slot.
    eval(&mut ctx, "(fset 'fn-gc (lambda () 42))");
    // Force GC.
    ctx.gc_collect();
    // Call it — must survive.
    assert_eq!(eval(&mut ctx, "(funcall 'fn-gc)"), Value::fixnum(42));
}
```

**Helper-API caveats:**
- Verify `Context::new()` supports `fset`, `fboundp`, `fmakunbound`, `funcall`, `lambda`. If any is missing from bare context, report BLOCKED with the specific form — don't weaken the test.
- `ctx.gc_collect()` is the existing helper used by other regression tests in this crate (see `symbol_plist_regression_test.rs` for reference).

- [ ] **Step 0.2: Register the module**

In `neovm-core/src/emacs_core/mod.rs`, find where `symbol_plist_regression_test` is registered:

```
grep -n 'symbol_plist_regression_test' neovm-core/src/emacs_core/mod.rs
```

Add a parallel registration for the new module, same pattern:

```rust
#[cfg(test)]
#[path = "symbol_function_regression_test.rs"]
mod symbol_function_regression_test;
```

- [ ] **Step 0.3: Run tests — expect all 5 pass**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_function_regression)' --no-fail-fast 2>&1 > /tmp/t0-fn.txt
tail -12 /tmp/t0-fn.txt
```

Expected: **5/5 pass**.

If `fset_to_nil_is_unbound` fails today (i.e., NeoMacs currently treats `(fset 'foo nil)` as a bound function with value nil), that's a pre-existing divergence from GNU. Flag in report — either fix it as part of T1 or document as a known gap.

- [ ] **Step 0.4: Commit**

```
git add neovm-core/src/emacs_core/symbol_function_regression_test.rs neovm-core/src/emacs_core/mod.rs
git commit -m "symbol-function: add regression tests for GNU-parity

Five tests documenting the symbol function slot's GNU-equivalent
semantics (fboundp, fset, fmakunbound, nil-as-unbound, GC survival).
All pass today; gate for the upcoming Option<Value> → Value flip."
```

---

## Task 1: Flip `LispSymbol::function: Option<Value>` → `Value`

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`
- Modify: `neovm-core/src/emacs_core/eval.rs`
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs` (dump/load paths)
- Modify: `neovm-core/src/emacs_core/pdump/types.rs` (field type — version bump is T2)
- Modify: test files that inspect `sym.function` directly

### Concrete changes

- [ ] **Step 1.1: Change the struct field**

In `neovm-core/src/emacs_core/symbol.rs`, find the `LispSymbol` struct around line 258:

```rust
pub function: Option<Value>,
```

Replace with:

```rust
/// Function slot. `Value::NIL` is the unbound sentinel (GNU `Qnil` in
/// `struct Lisp_Symbol::s.function`, `lisp.h:820`).
pub function: Value,
```

- [ ] **Step 1.2: Update the struct constructor**

Find every `LispSymbol { ... }` literal construction:

```
grep -n 'function: None\|function: Some(' neovm-core/src/emacs_core/symbol.rs
```

Change `function: None,` → `function: Value::NIL,`. For any `function: Some(v),` → `function: v,`.

- [ ] **Step 1.3: Rewrite `fboundp_id`**

Find (around line 1475):

```rust
pub fn fboundp_id(&self, id: SymId) -> bool {
    self.slot(id)
        .filter(|sym| !sym.function_unbound)
        .and_then(|s| s.function.as_ref())
        .is_some()
}
```

Replace with:

```rust
pub fn fboundp_id(&self, id: SymId) -> bool {
    self.slot(id)
        .is_some_and(|s| !s.function_unbound && !s.function.is_nil())
}
```

- [ ] **Step 1.4: Rewrite `fmakunbound_id`**

Find (around line 1381):

```rust
pub fn fmakunbound_id(&mut self, id: SymId) {
    let Some(sym) = self.slot_mut(id) else { return; };
    let mut changed = !sym.function_unbound;
    sym.function_unbound = true;
    changed |= sym.function.take().is_some();
    if changed {
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }
}
```

Replace with:

```rust
pub fn fmakunbound_id(&mut self, id: SymId) {
    let Some(sym) = self.slot_mut(id) else { return; };
    let was_unbound = sym.function_unbound;
    let was_bound = !sym.function.is_nil();
    sym.function_unbound = true;
    sym.function = Value::NIL;
    if !was_unbound || was_bound {
        self.function_epoch = self.function_epoch.wrapping_add(1);
    }
}
```

- [ ] **Step 1.5: Locate and rewrite the other fmakunbound-style variant**

Around line 1400 there's a block like:

```rust
if sym.function.take().is_some() {
    self.function_epoch = self.function_epoch.wrapping_add(1);
}
```

This pattern appears where an mutator wants to "clear the function if present". Replace with:

```rust
if !sym.function.is_nil() {
    sym.function = Value::NIL;
    self.function_epoch = self.function_epoch.wrapping_add(1);
}
```

### Step 1.6: Rewrite the `fset`-style writers

Find (around lines 1360-1372):

```rust
sym.function = Some(function);
sym.function_unbound = false;
self.function_epoch = self.function_epoch.wrapping_add(1);
```

Replace each with:

```rust
sym.function = function;
sym.function_unbound = false;
self.function_epoch = self.function_epoch.wrapping_add(1);
```

(Simply drop the `Some(…)` wrap.)

- [ ] **Step 1.7: Rewrite `sym.function.as_ref()` read sites**

```
grep -n 'sym\.function\.as_ref\|\.function\.as_ref()' neovm-core/src/emacs_core/symbol.rs
```

Known sites:
- Around line 1340 (`fset_id` read-old-value): `sym.function.as_ref()` → fix the whole block.
- Around line 1478 (`visible_symbol_function_id` or similar): `.and_then(|s| s.function.as_ref())` → `.filter(|sym| !sym.function_unbound).map(|s| s.function).filter(|f| !f.is_nil())`.
- Around line 1856 (`indirect_function_id`): `self.slot(current_id)?.function.as_ref()?` → rewrite to return `Value` with NIL check.

Specific rewrites:

Site 1478 (visible function):
```rust
// Before:
.filter(|sym| !sym.function_unbound)
.and_then(|s| s.function.as_ref())
```
```rust
// After:
.filter(|sym| !sym.function_unbound && !sym.function.is_nil())
.map(|s| s.function)
```

(Or if the surrounding signature expects `Option<Value>`, keep the `.map` returning a Value-by-value Option. Verify the function's return type.)

Site 1856 (indirect_function loop):
```rust
// Before:
let func = self.slot(current_id)?.function.as_ref()?;
```
```rust
// After:
let func = self.slot(current_id).map(|s| s.function).filter(|f| !f.is_nil())?;
```

- [ ] **Step 1.8: Rewrite GC trace**

Find (around line 2000):

```rust
if let Some(f) = sym.function {
    roots.push(f);
}
```

Replace with:

```rust
roots.push(sym.function);
```

(Pushing `Value::NIL` is harmless — NIL is an immediate tagged value, no heap ref.)

- [ ] **Step 1.9: Update `eval.rs` caller**

```
grep -n 'symbol\.function\.as_ref\|sym\.function\.as_ref' neovm-core/src/emacs_core/eval.rs
```

Around line 4186 there's:

```rust
match symbol.function.as_ref() {
    Some(f) => ...,
    None => ...,
}
```

Read the surrounding context to understand what the branches do. Replace with an `is_nil` check:

```rust
if symbol.function.is_nil() {
    // (was None branch)
} else {
    let f = symbol.function;
    // (was Some(f) branch)
}
```

Adapt to match whatever the original match arms did.

- [ ] **Step 1.10: Update `pdump/types.rs`**

In `neovm-core/src/emacs_core/pdump/types.rs`, find (around line 343):

```rust
pub function: Option<DumpValue>,
```

Change to:

```rust
pub function: DumpValue,
```

**Do not touch** `pub set_function: Option<DumpValue>,` and `pub get_function: Option<DumpValue>,` at lines 791-792 — those are in `DumpCustomWatcher` / different struct, unrelated to symbol function slot.

- [ ] **Step 1.11: Update `pdump/convert.rs` dump/load**

```
grep -n 'sym\.function\|symbol\.function\|function:\s*sym' neovm-core/src/emacs_core/pdump/convert.rs
```

Find `dump_symbol_data`:
```rust
function: sym.function.map(|f| encoder.dump_value(f)),
```
Replace with:
```rust
function: encoder.dump_value(sym.function),
```

Find `load_symbol_data`:
```rust
symbol.function = sd.function.as_ref().map(|f| decoder.load_value(f));
```
Replace with:
```rust
symbol.function = decoder.load_value(&sd.function);
```

(Exact syntax depends on the existing encoder/decoder method names — adapt to match.)

- [ ] **Step 1.12: Update tests inspecting `sym.function` directly**

```
grep -rn 'sym\.function\|symbol\.function\|\.function\.as_ref\|\.function\.clone\|\.function\.map\|\.function\.is_some\|\.function\.is_none' neovm-core/src --include='*_test.rs' 2>&1 | head -20
```

For each hit:
- `sym.function.as_ref() == Some(&v)` → `sym.function == v && !sym.function.is_nil()` (or equivalent).
- `sym.function.is_some()` → `!sym.function.is_nil()`.
- `sym.function.is_none()` → `sym.function.is_nil()`.
- `sym.function.clone()` → `sym.function` (Copy).

- [ ] **Step 1.13: `cargo check`**

```
cargo check -p neovm-core
```

Fix any remaining compile errors. Common:
- A missed `sym.function.as_ref()` or `.clone()`.
- A test assertion still using `Some(v)`.

- [ ] **Step 1.14: Regression harness**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_function_regression)' --no-fail-fast 2>&1 > /tmp/t1-reg.txt
tail -10 /tmp/t1-reg.txt
```

Expected: **5/5 pass**.

- [ ] **Step 1.15: Targeted sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol::) or test(fboundp) or test(fmakunbound) or test(fset) or test(symbol_function) or test(symbol_plist_regression) or test(symbol_redirect_regression)' --no-fail-fast 2>&1 > /tmp/t1-sweep.txt
tail -8 /tmp/t1-sweep.txt
grep -c '^\s*FAIL ' /tmp/t1-sweep.txt || true
```

Expected: no new failures vs pre-T1 baseline.

- [ ] **Step 1.16: Commit**

```
git add neovm-core/src/emacs_core/symbol.rs neovm-core/src/emacs_core/eval.rs neovm-core/src/emacs_core/pdump/convert.rs neovm-core/src/emacs_core/pdump/types.rs
git add -u  # catch any test file updates
git commit -m "symbol: flip LispSymbol::function to Value with NIL sentinel

Replaces Option<Value> with bare Value using Value::NIL as the unbound
sentinel, matching GNU struct Lisp_Symbol::s.function (lisp.h:820).
fboundp_id checks !function_unbound && !function.is_nil();
fmakunbound_id writes NIL + sets unbound flag. GC trace pushes the
single Value root.

Pdump DumpSymbolData::function is also now DumpValue (format version
not yet bumped — T2). Part of the GNU-shape symbol follow-ups."
```

---

## Task 2: Pdump format v22 → v23

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/mod.rs` (version constant)
- Modify: `neovm-core/src/emacs_core/pdump/pdump_test.rs` (if any hardcoded v22)

- [ ] **Step 2.1: Locate the format version**

```
grep -rn 'FORMAT_VERSION\|DUMP_FORMAT_VERSION' neovm-core/src/emacs_core/pdump --include='*.rs'
```

Find `FORMAT_VERSION` — currently 22.

- [ ] **Step 2.2: Bump to 23**

Change the constant to 23. Update the adjacent version-history comment:

```rust
/// Dump format version. Bumped by:
/// - v22: LispSymbol::plist flipped to Value cons list.
/// - v23: LispSymbol::function flipped to Value with NIL sentinel (DumpSymbolData::function is now DumpValue).
pub const FORMAT_VERSION: u32 = 23;
```

(Preserve whatever earlier-version history comment already exists.)

- [ ] **Step 2.3: Update test fixtures**

```
grep -rn '22\b' neovm-core/src/emacs_core/pdump/pdump_test.rs | head
```

If any test asserts `FORMAT_VERSION == 22` literally, update to 23. Ignore 22 in unrelated numeric contexts (e.g. byte counts, timeouts).

- [ ] **Step 2.4: `cargo check`**

```
cargo check -p neovm-core
```

- [ ] **Step 2.5: Run pdump + regression**

```
cargo nextest run -p neovm-core --lib -E 'test(pdump::) or test(symbol_function_regression)' --no-fail-fast 2>&1 > /tmp/t2-pd.txt
tail -12 /tmp/t2-pd.txt
grep -c '^\s*FAIL ' /tmp/t2-pd.txt || true
```

Expected: 5/5 regression pass; pre-existing `test_measure_current_workspace_*` failures remain (pre-existing); no new failures.

- [ ] **Step 2.6: Commit**

```
git add neovm-core/src/emacs_core/pdump/mod.rs neovm-core/src/emacs_core/pdump/pdump_test.rs
git commit -m "pdump: bump format v22→v23 for symbol function as Value

DumpSymbolData::function flipped from Option<DumpValue> to DumpValue
in T1. Bump the format version so existing v22 dumps are rejected
instead of silently misloaded. Old dumps are regenerated via cargo
xtask (project memory S105 — no backward compat required).
Completes the symbol-function-as-value refactor."
```

---

## Post-plan validation

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_function_regression) or test(symbol_plist_regression) or test(symbol_redirect_regression)' --no-fail-fast 2>&1 > /tmp/final-fn.txt
tail -15 /tmp/final-fn.txt
```

Expected: 5 + 7 + 6 = **18/18 regression tests pass** across the three symbol-refactor harnesses.

Success-criteria greps:

```
grep -rn 'function: Option<Value>\|function: Option<DumpValue>' neovm-core/src --include='*.rs' | grep -v 'set_function\|get_function'
grep -rn '\.function\.as_ref\|\.function\.clone\|\.function\.is_some\|\.function\.is_none\|\.function = Some\|\.function = None' neovm-core/src --include='*.rs'
```

Expected: zero hits on both (except the `set_function` / `get_function` fields in `DumpCustomWatcher` — those are unrelated).

---

## Self-review

**Spec coverage:**

| Spec section | Plan task |
|---|---|
| §Architecture — field type change to Value | Task 1 steps 1.1-1.2 |
| §Architecture — NIL sentinel | Task 1 steps 1.3-1.9 |
| §Architecture — GC trace | Task 1 step 1.8 |
| §Architecture — pdump shape change | Task 1 steps 1.10-1.11 |
| §Testing — 5 regression tests | Task 0 |
| §Migration Phase A (harness) | Task 0 |
| §Migration Phase B (flip) | Task 1 |
| §Migration Phase C (pdump version) | Task 2 |

Coverage complete.

**Placeholder scan:** No "TBD" / "similar to" / "TODO" in the plan.

**Type consistency:** `Value::NIL`, `is_nil()`, `function_unbound` used consistently. The `epoch` ordering on `function_unbound` change preserved from existing code. Pdump field name `function: DumpValue` referenced in T1.10 and T1.11.

**Ordering:** Task 0 gate before 1. Task 2 bumps version after T1's shape change.
