# Delete Legacy `SymbolValue` Enum Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete NeoMacs's legacy `SymbolValue` enum and `LispSymbol::value` field, so the symbol value-cell shape bit-for-bit matches GNU `struct Lisp_Symbol::s.val` (`lisp.h:810-816`).

**Architecture:** Switch every read/write of symbol value state to dispatch on `flags.redirect()` + `sym.val` union. Collapse the 3-pass lookup in `vm.rs::lookup_var_id` and `eval.rs::eval_symbol_by_id` to a single redirect-dispatch. Move the GC trace from the legacy enum to the `val.plain` union field. Pdump format bumps v11 → v12.

**Tech Stack:** Rust (stable 1.93.1), `cargo nextest` for tests. Target crate `neovm-core`.

**Spec reference:** `docs/superpowers/specs/2026-04-17-delete-symbolvalue-design.md`

**Testing conventions (project-wide):**
- Use `cargo nextest run -p neovm-core <test_name>` — never `cargo test` (`feedback_cargo_nextest`).
- Use `cargo check -p neovm-core` for compile validation during dev. Never `--release` (`feedback_no_release_build`).

**Line-number note:** line numbers cited in early tasks drift as later tasks patch the same files. Implementers should grep for identifiers (`fn lookup_var_id`, `pub enum SymbolValue`, etc.) rather than trust absolute line numbers. The file paths are stable.

**Baseline before starting:**
- Branch from `main` at current HEAD.
- `cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -5` baseline is ~91 failing tests (documented in project memory). "No regression" = no new failures relative to this baseline.

---

## Task 0: Write the regression-test harness

**Rationale:** The six correctness tests from the spec should pass today and must continue to pass after every phase. Write them first as a gate.

**Files:**
- Create: `neovm-core/src/emacs_core/symbol_redirect_regression_test.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs` (add the module)

- [ ] **Step 0.1: Write the six regression tests**

Create `neovm-core/src/emacs_core/symbol_redirect_regression_test.rs`:

```rust
//! Regression tests gating the Phase 10 SymbolValue deletion refactor.
//! Every test here must pass on every phase of the refactor.

use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;
use crate::tagged::value::TaggedValue;

fn eval(ctx: &mut Context, src: &str) -> Value {
    let expr = crate::emacs_core::reader::read_from_string(src)
        .expect("read");
    ctx.eval_value(&expr).expect("eval")
}

#[test]
fn plainval_void_after_makunbound_signals_void_variable() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(setq foo-phase10-a 42)");
    assert_eq!(
        eval(&mut ctx, "foo-phase10-a"),
        Value::fixnum(42)
    );
    eval(&mut ctx, "(makunbound 'foo-phase10-a)");
    let err = crate::emacs_core::reader::read_from_string("foo-phase10-a")
        .and_then(|e| ctx.eval_value(&e).map_err(|f| format!("{:?}", f)));
    assert!(
        err.is_err(),
        "reading unbound symbol should signal void-variable, got {:?}",
        err
    );
}

#[test]
fn plainval_nil_is_distinct_from_unbound() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(setq foo-phase10-b nil)");
    assert_eq!(eval(&mut ctx, "foo-phase10-b"), Value::NIL);
    // (boundp 'foo-phase10-b) should be t: bound-to-nil is still bound.
    assert_eq!(eval(&mut ctx, "(boundp 'foo-phase10-b)"), Value::T);
}

#[test]
fn cross_buffer_localized_isolation_matches_gnu() {
    // Documented in memory: GNU 31 returns (9 1) for this form.
    let mut ctx = Context::new();
    let result = eval(
        &mut ctx,
        "(progn (setq vm-mlv-preserve-global 1)
                (with-temp-buffer
                  (set (make-local-variable 'vm-mlv-preserve-global) 9)
                  (make-local-variable 'vm-mlv-preserve-global)
                  (list vm-mlv-preserve-global
                        (default-value 'vm-mlv-preserve-global))))",
    );
    // Result is (9 1). Stringify to compare.
    let printed = crate::emacs_core::print::print_to_string(&result, &ctx);
    assert_eq!(printed, "(9 1)");
}

#[test]
fn varalias_chain_forwards_reads_and_writes() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(setq phase10-c 100)");
    eval(&mut ctx, "(defvaralias 'phase10-b 'phase10-c)");
    eval(&mut ctx, "(defvaralias 'phase10-a 'phase10-b)");
    assert_eq!(eval(&mut ctx, "phase10-a"), Value::fixnum(100));
    eval(&mut ctx, "(setq phase10-a 200)");
    assert_eq!(eval(&mut ctx, "phase10-c"), Value::fixnum(200));
}

#[test]
fn forwarded_conditional_slot_fill_column_works() {
    let mut ctx = Context::new();
    eval(&mut ctx, "(setq-default fill-column 70)");
    let v = eval(
        &mut ctx,
        "(with-temp-buffer
           (make-local-variable 'fill-column)
           (setq fill-column 40)
           (list fill-column (default-value 'fill-column)))",
    );
    let printed = crate::emacs_core::print::print_to_string(&v, &ctx);
    assert_eq!(printed, "(40 70)");
}

#[test]
fn plainval_survives_forced_gc() {
    let mut ctx = Context::new();
    // Build a fresh cons in a symbol value, force GC, read it back.
    eval(&mut ctx, "(setq phase10-gc-test (cons 1 2))");
    let before = eval(&mut ctx, "phase10-gc-test");
    ctx.force_gc_for_test();
    let after = eval(&mut ctx, "phase10-gc-test");
    // The cons may move, but its car/cdr must be preserved.
    let car_before = crate::emacs_core::value::eq_value(&before.cons_car(), &Value::fixnum(1));
    let cdr_after = crate::emacs_core::value::eq_value(&after.cons_cdr(), &Value::fixnum(2));
    assert!(car_before, "car should be 1 before GC");
    assert!(cdr_after, "cdr should be 2 after GC — stale if GC trace missed it");
}
```

**Note:** if the helper `Context::force_gc_for_test` or `print_to_string` has a different name in this codebase, grep for the closest equivalent and substitute. Likewise for `Context::new` — if the bare constructor lacks defun/make-local-variable, use the bootstrap constructor (see `feedback_context_new_is_bare`). As of today, `Context::new()` is bare; use the bootstrap pathway if needed.

- [ ] **Step 0.2: Register the module**

In `neovm-core/src/emacs_core/mod.rs`, find the block where test modules are declared (look for `mod symbol_test;` or similar `#[cfg(test)] mod foo_test;` lines) and add:

```rust
#[cfg(test)]
mod symbol_redirect_regression_test;
```

- [ ] **Step 0.3: Run tests — expect all pass today**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
```

Expected: **6 passed, 0 failed**. If any fail today, the helpers or bootstrap path needs adjustment before continuing. Fix helper invocation until all pass. Do NOT continue to Task 1 with any of these red.

- [ ] **Step 0.4: Commit**

```
git add neovm-core/src/emacs_core/symbol_redirect_regression_test.rs neovm-core/src/emacs_core/mod.rs
git commit -m "symbol: add Phase 10 SymbolValue-deletion regression tests

Six correctness gates from docs/superpowers/specs/2026-04-17-delete-
symbolvalue-design.md §Testing. These must pass at every stage of the
Phase 10 refactor that follows."
```

---

## Task 1: Phase A — switch `SymbolVal::default` to `UNBOUND`, collapse `find_symbol_value::Plainval` arm

**Rationale:** Make `val.plain` authoritative for "unbound" semantics (via `Value::UNBOUND` sentinel) so the legacy `SymbolValue::Plain(None)` match can be retired.

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`
- Test: relies on Task 0 regression tests.

- [ ] **Step 1.1: Change `SymbolVal::default()`**

Find `impl Default for SymbolVal` in `neovm-core/src/emacs_core/symbol.rs` (near line 186). Replace:

```rust
impl Default for SymbolVal {
    fn default() -> Self {
        // Plainval / NIL is the safe initial state.
        Self { plain: Value::NIL }
    }
}
```

with:

```rust
impl Default for SymbolVal {
    fn default() -> Self {
        // Plainval / UNBOUND is the correct initial state — matches GNU
        // where freshly-interned symbols have val.value == Qunbound.
        Self { plain: Value::UNBOUND }
    }
}
```

- [ ] **Step 1.2: Audit all `sym.val.plain = Value::NIL` writes**

Run:
```
grep -rn 'val\.plain = Value::NIL\|val\.plain = Value::nil\|sym\.val = SymbolVal { plain: Value::NIL' neovm-core/src
```

For each hit that represents "mark symbol as unbound", change to `Value::UNBOUND`. For writes that legitimately mean "set to nil" (e.g., from `(setq foo nil)`), leave as `Value::NIL` — these are distinct semantically.

Known mutation sites to review:
- `ensure_slot` (near line 260) — initial state should be UNBOUND.
- `set_symbol_value_id_inner` (near line 1277) — no change; caller supplies value.
- `bootstrap_nil` / `bootstrap_t` (near 654-669) — NIL's value is itself `Value::NIL`, T's value is `Value::T`; no change.

- [ ] **Step 1.3: Collapse `find_symbol_value::Plainval` arm**

Find `pub fn find_symbol_value(&self, id: SymId) -> Option<Value>` in `symbol.rs` (currently near line 972). Replace the `SymbolRedirect::Plainval` arm:

```rust
SymbolRedirect::Plainval => {
    // Phase 2: read through the legacy `value` field for
    // the bound check. The new `val.plain` mirror agrees
    // (every internal mutator keeps both in sync). Phase 4
    // collapses to `val.plain != Value::UNBOUND`.
    match sym.value {
        SymbolValue::Plain(v) => return v,
        SymbolValue::BufferLocal { default, .. } => return default,
        SymbolValue::Alias(target) => {
            current = target;
            continue;
        }
        SymbolValue::Forwarded => return None,
    }
}
```

with:

```rust
SymbolRedirect::Plainval => {
    // Read `val.plain` directly. UNBOUND sentinel means void.
    let v = unsafe { sym.val.plain };
    if v == Value::UNBOUND {
        return None;
    }
    return Some(v);
}
```

- [ ] **Step 1.4: Run the regression harness**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
```

Expected: **6 passed**. If `plainval_void_after_makunbound_signals_void_variable` fails, check that `makunbound` writes `Value::UNBOUND` to `val.plain`.

- [ ] **Step 1.5: Run full buffer+symbol+eval module sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(buffer) or test(eval) or test(custom) or test(bytecode)' --no-fail-fast 2>&1 | tail -6
```

Expected: no new failures relative to baseline. Record pre-Phase-A failure count for reference.

- [ ] **Step 1.6: Commit**

```
git add neovm-core/src/emacs_core/symbol.rs
git commit -m "symbol: Phase A — switch val.plain to UNBOUND sentinel

SymbolVal::default() initializes plain=UNBOUND instead of NIL so the
distinction between 'bound to nil' and 'unbound' is representable in
the new union. find_symbol_value's Plainval arm now reads val.plain
directly via the UNBOUND sentinel, bypassing the legacy SymbolValue
enum. Part of Phase 10 of the symbol-redirect refactor."
```

---

## Task 2: Phase B — delete Pass 2 in `vm.rs`

**Rationale:** The legacy buffer-local fallback in `lookup_var_id` / `assign_var_id` / `set-default` is redundant with Pass 1 (redirect-tag dispatch). Every symbol that `is_buffer_local_id` returns true for is already handled by Pass 1 since `make-variable-buffer-local` flips the redirect to `Localized`.

**Files:**
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs`

- [ ] **Step 2.1: Delete Pass 2 in `lookup_var_id`**

Find `fn lookup_var_id` in `vm.rs` (currently near line 1898). Locate the block starting with the comment:

```rust
// Phase 2 fall-back: legacy buffer-local detour for symbols
// still on the legacy storage path. Gated on
// `is_buffer_local_id` so the String allocation + HashMap
// lookup only fires for marked buffer-local variables.
let is_local = self.ctx.obarray.is_buffer_local_id(resolved)
    || self.ctx.custom.is_auto_buffer_local_symbol(resolved);
if is_local
    && crate::emacs_core::builtins::is_canonical_symbol_id(resolved)
    && let Some(buf) = self.ctx.buffers.current_buffer()
{
    let resolved_name = resolve_sym(resolved);
    if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
        return binding
            .as_value()
            .or_else(|| {
                (resolved_name == "buffer-undo-list")
                    .then(|| buf.buffer_local_value(resolved_name))
                    .flatten()
            })
            .ok_or_else(|| signal("void-variable", vec![Value::from_sym_id(name_id)]));
    }
}
```

Delete the entire block (from the `// Phase 2 fall-back:` comment through the closing brace of the `if is_local && ... { ... }` block).

Also update the comment at the top of Pass 1 (around line 1895) that mentions Pass 2 — change "we fall through to the legacy buffer-local detour and the PLAINVAL fast path" to "we fall through to the PLAINVAL fast path via `find_symbol_value`".

- [ ] **Step 2.2: Delete the analogous Pass 2 in `assign_var_id`**

Find `fn assign_var_id` in `vm.rs` (currently near line 2010). Locate the same `is_buffer_local_id || is_auto_buffer_local_symbol` gate (currently around line 2154) and remove the wrapped block the same way as 2.1.

- [ ] **Step 2.3: Fix `set-default` builtin redirect check**

Find the `set-default` builtin implementation in `vm.rs` (currently near line 2389). Locate:

```rust
let is_buffer_local = self.ctx.obarray.is_buffer_local(resolved_name)
    || self.ctx.custom.is_auto_buffer_local_symbol(resolved);
```

Replace with a redirect-tag check:

```rust
use crate::emacs_core::symbol::SymbolRedirect;
let is_buffer_local = self
    .ctx
    .obarray
    .get_by_id(resolved)
    .is_some_and(|s| s.flags.redirect() == SymbolRedirect::Localized);
```

Remove the now-unused `resolved_name` binding at the top of the function if it has no other uses.

- [ ] **Step 2.4: Run regression + module sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(buffer) or test(eval) or test(custom) or test(bytecode)' --no-fail-fast 2>&1 | tail -6
```

Expected: 6/6 regression tests pass. No new failures in the sweep. The `cross_buffer_localized_isolation_matches_gnu` test is the specific gate for this phase.

- [ ] **Step 2.5: Commit**

```
git add neovm-core/src/emacs_core/bytecode/vm.rs
git commit -m "vm: Phase B — delete legacy buffer-local Pass 2 from hot paths

lookup_var_id and assign_var_id had a Pass 2 fallback gated on the
legacy is_buffer_local_id + is_auto_buffer_local_symbol markers that
read the same data Pass 1 already consulted. With make-local-variable
flipping the redirect to Localized, Pass 1 handles every buffer-local
symbol. Also redirects the set-default builtin's branch check to the
redirect tag. Part of Phase 10 of the symbol-redirect refactor."
```

---

## Task 3: Phase C — delete Pass 2 in `eval.rs`

**Rationale:** Same cleanup as Task 2 but in the non-VM eval path. Three or four call sites.

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs`

- [ ] **Step 3.1: Grep for the legacy-marker sites**

```
grep -n 'is_buffer_local_id\|is_auto_buffer_local_symbol\|is_buffer_local' neovm-core/src/emacs_core/eval.rs
```

Known sites from prior grep (line numbers drift): 10132, 10622, 10664. Plus any in `eval_symbol_by_id` (near 6681) and `set_runtime_binding`.

- [ ] **Step 3.2: Update each site to use redirect-tag check**

For each `obarray.is_buffer_local(...)` or `obarray.is_buffer_local_id(...)` or `custom.is_auto_buffer_local_symbol(...)` check, replace with:

```rust
use crate::emacs_core::symbol::SymbolRedirect;
let is_local = ctx.obarray
    .get_by_id(resolved)
    .is_some_and(|s| s.flags.redirect() == SymbolRedirect::Localized);
```

(Adjust `ctx` to whatever receiver is in scope at each site — could be `self.ctx`, `ctx`, `state`, or a direct `&Obarray`.)

For any site where the check is combined in an `||` with `custom.is_auto_buffer_local_symbol`, drop the `||` side entirely — both paths represent the same "is this LOCALIZED" question and the redirect tag is authoritative.

Where a site is a "fallback to legacy data" block (mirrors the Pass 2 in vm.rs), delete the entire fallback block, not just the gate.

- [ ] **Step 3.3: `eval_symbol_by_id` specifically**

Find `pub(crate) fn eval_symbol_by_id` (near line 6681). It contains both a `read_localized` call and a fallback to `get_buffer_local_binding_by_sym_id`. After the redirect-routed read in `read_localized`, the fallback is redundant for LOCALIZED symbols. Delete the fallback block and any `is_auto_buffer_local_symbol` gate above it.

- [ ] **Step 3.4: Run regression + sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib -E 'test(symbol) or test(buffer) or test(eval) or test(custom) or test(bytecode)' --no-fail-fast 2>&1 | tail -6
```

Expected: 6/6 regression. No new failures.

- [ ] **Step 3.5: Commit**

```
git add neovm-core/src/emacs_core/eval.rs
git commit -m "eval: Phase C — delete legacy buffer-local fallbacks

Mirrors Phase B for the eval.rs paths. eval_symbol_by_id and
set_runtime_binding no longer consult the legacy is_buffer_local_id /
is_auto_buffer_local_symbol markers; branch decisions now come from
the redirect tag. Part of Phase 10 of the symbol-redirect refactor."
```

---

## Task 4: Phase D — delete `CustomManager::auto_buffer_local`

**Rationale:** The `HashSet<SymId>` mirror of the legacy `BufferLocal` marker has no remaining readers after Tasks 2 and 3.

**Files:**
- Modify: `neovm-core/src/emacs_core/custom.rs`

- [ ] **Step 4.1: Verify no remaining readers**

```
grep -rn 'auto_buffer_local\|make_variable_buffer_local_symbol\|is_auto_buffer_local_symbol' neovm-core/src --include='*.rs'
```

Expected: only definitions in `custom.rs` and the `custom.make_variable_buffer_local_symbol(resolved_id)` call in `custom.rs:131`. If any `is_auto_buffer_local_symbol` reads remain elsewhere, fold them into Task 3.

- [ ] **Step 4.2: Delete the field and its methods**

In `neovm-core/src/emacs_core/custom.rs`, find the `CustomManager` struct and delete:

- The `auto_buffer_local: FxHashSet<SymId>` field.
- The `make_variable_buffer_local_symbol` method.
- The `is_auto_buffer_local` method.
- The `is_auto_buffer_local_symbol` method.

Update `CustomManager::new()` / `default()` to not initialize the deleted field.

- [ ] **Step 4.3: Remove the legacy calls from `builtin_make_variable_buffer_local_with_state`**

Find the function (near line 95). Delete these two lines:

```rust
obarray.make_buffer_local(&resolved, true);
custom.make_variable_buffer_local_symbol(resolved_id);
```

The `make_symbol_localized` + `set_blv_local_if_set` calls above them are the authoritative path.

Update the comment above at lines 121-126 to remove the "The legacy BufferLocal SymbolValue marker and the CustomManager auto_buffer_local set stay in sync until Phase 10 deletes them" sentence — Phase 10 is this task.

- [ ] **Step 4.4: Drop `Obarray::make_buffer_local` if now unused**

```
grep -rn 'make_buffer_local\b' neovm-core/src --include='*.rs'
```

If the only remaining definition is in `symbol.rs` and the only call-site was the one just deleted, also delete the method in `symbol.rs` (near line 1655).

- [ ] **Step 4.5: Run sweep**

```
cargo check -p neovm-core
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib -E 'test(custom) or test(symbol) or test(buffer)' --no-fail-fast 2>&1 | tail -6
```

Expected: clean compile, 6/6 regression, no new failures.

- [ ] **Step 4.6: Commit**

```
git add neovm-core/src/emacs_core/custom.rs neovm-core/src/emacs_core/symbol.rs
git commit -m "custom: Phase D — delete auto_buffer_local mirror

The HashSet<SymId> was a pure mirror of the legacy BufferLocal marker.
Tasks 2-3 removed the last readers. Also removes the dead
Obarray::make_buffer_local method whose only caller was the Phase D
cleanup. Part of Phase 10 of the symbol-redirect refactor."
```

---

## Task 5: Phase E — move GC trace to `val.plain` via redirect dispatch

**Rationale:** GC soundness. This is the highest-risk phase: once the trace source changes, any Plainval symbol whose `val.plain` isn't up-to-date will have its value freed. Tasks 1-4 migrated readers to `val`; this task confirms `val` is authoritative for the GC's purposes.

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`

- [ ] **Step 5.1: Replace `Obarray::trace_roots` body**

Find `impl GcTrace for Obarray` (near line 1970). Replace the entire `fn trace_roots` body with:

```rust
fn trace_roots(&self, roots: &mut Vec<Value>) {
    for sym in self.symbols.iter().flatten() {
        match sym.flags.redirect() {
            SymbolRedirect::Plainval => {
                // Safety: redirect==Plainval means val.plain is the
                // live variant. Tagged pointers are Copy.
                let v = unsafe { sym.val.plain };
                if v != Value::UNBOUND {
                    roots.push(v);
                }
            }
            // Varalias → val.alias is a SymId (not a heap ref).
            // Forwarded → val.fwd is static data.
            // Localized → BLV traced separately below.
            SymbolRedirect::Varalias
            | SymbolRedirect::Forwarded
            | SymbolRedirect::Localized => {}
        }
        if let Some(ref f) = sym.function {
            roots.push(*f);
        }
        for pval in sym.plist.values() {
            roots.push(*pval);
        }
    }
    // BLV contents remain traced via the existing blvs pool — unchanged.
    for &blv_ptr in &self.blvs {
        let blv = unsafe { &*blv_ptr };
        roots.push(blv.defcell);
        roots.push(blv.valcell);
        roots.push(blv.where_buf);
    }
}
```

- [ ] **Step 5.2: Run the GC regression test specifically**

```
cargo nextest run -p neovm-core --lib -E 'test(plainval_survives_forced_gc)' --no-fail-fast
```

Expected: PASS. This is the most important single test in the refactor. If it fails, there is a writer that sets `sym.value = SymbolValue::Plain(Some(v))` but doesn't set `sym.val.plain = v`. Revert this commit and audit writers before retrying.

- [ ] **Step 5.3: Run full crate test sweep**

```
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
```

Expected: same pass/fail count as baseline (~91 failing). Any NEW failure is a regression from this phase and must be investigated — most likely a missing dual-write somewhere.

- [ ] **Step 5.4: Commit**

```
git add neovm-core/src/emacs_core/symbol.rs
git commit -m "symbol: Phase E — GC trace reads val.plain via redirect

Obarray::trace_roots now walks the new SymbolVal union dispatched by
flags.redirect(), instead of the legacy SymbolValue enum. Matches GNU's
mark_object handling in alloc.c. Part of Phase 10 of the symbol-
redirect refactor.

High-risk phase: any missed val.plain write is now a silent
use-after-free. The plainval_survives_forced_gc regression test is
the gate."
```

---

## Task 6: Phase F — remove `sym.value` writes inside `symbol.rs`

**Rationale:** With all readers migrated off `sym.value`, the field has no consumers. Stop writing to it.

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`

- [ ] **Step 6.1: Find every `sym.value = ...` or `s.value = ...` write in symbol.rs**

```
grep -n 'sym\.value = \|self\.value = \|s\.value = \|\.value = SymbolValue::' neovm-core/src/emacs_core/symbol.rs
```

Known sites:
- `bootstrap_nil` (near 665)
- `bootstrap_t` (near 654)
- `define_keyword` / keyword interning block (near 591)
- `ensure_slot` / initial value (near 407)
- `make_symbol_localized` (near 829)
- `install_buffer_objfwd` (near 957)
- `set_symbol_value_id_inner` (near 1277) — may have multiple arms

- [ ] **Step 6.2: Delete each write**

For each hit, delete the assignment line. For sites that only existed to keep `value` in sync with `val`, the deletion is straightforward — the `val` write already happens adjacent to it.

For `set_symbol_value_id_inner`: this function has branches per redirect. The `SymbolValue::BufferLocal` arm that clobbers redirect to PLAINVAL (documented in the memory as a bug deliberately kept to match legacy semantics) should be deleted entirely. The function now only writes `sym.val` via the appropriate redirect arm.

- [ ] **Step 6.3: Verify no writes remain**

```
grep -n 'sym\.value = \|\.value = SymbolValue::' neovm-core/src/emacs_core/symbol.rs
```

Expected: zero hits (outside any `impl Default for SymbolValue` or similar, which goes away in Task 8).

- [ ] **Step 6.4: Run full sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
```

Expected: 6/6 regression, no new failures.

- [ ] **Step 6.5: Commit**

```
git add neovm-core/src/emacs_core/symbol.rs
git commit -m "symbol: Phase F — remove all sym.value writes

Every internal mutator that previously dual-wrote sym.value alongside
sym.val now writes only val via the appropriate redirect arm. The
field is still present but has no writers. Part of Phase 10 of the
symbol-redirect refactor."
```

---

## Task 7: Phase G — remove `sym.value` reads outside `symbol.rs`

**Rationale:** The legacy field has 4 scattered external readers. Route each through the redirect-tag + val-union dispatch.

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs`, `neovm-core/src/emacs_core/builtins/misc_eval.rs`, `neovm-core/src/emacs_core/interactive_test.rs`, `neovm-core/src/emacs_core/builtins/tests.rs`

- [ ] **Step 7.1: Grep for every remaining `SymbolValue::` outside symbol.rs**

```
grep -rn 'SymbolValue::' neovm-core/src --include='*.rs' | grep -v 'symbol.rs\|symbol_test.rs\|pdump/'
```

Expected: 4 hits (eval.rs, misc_eval.rs, interactive_test.rs, builtins/tests.rs). Plus maybe a line in `pdump/mod.rs` — that's Task 9's concern, ignore here.

- [ ] **Step 7.2: Update each call site**

For each of the 4 sites, replace the `SymbolValue::` pattern-match with a redirect-tag + val access. Common patterns:

- `SymbolValue::Plain(Some(v))` → `{ let v = unsafe { sym.val.plain }; v != Value::UNBOUND }` (for membership test)  or `Some(unsafe { sym.val.plain }).filter(|v| *v != Value::UNBOUND)` (for value extraction).
- `SymbolValue::Alias(target)` → with a check that `sym.flags.redirect() == SymbolRedirect::Varalias`, use `unsafe { sym.val.alias }`.
- `SymbolValue::BufferLocal { default, .. }` → use `obarray.blv(id).map(|b| b.defcell.cons_cdr())`.
- `SymbolValue::Forwarded` → check `sym.flags.redirect() == SymbolRedirect::Forwarded`.

If an accessor method doesn't exist for a given pattern, add a small helper on `LispSymbol` or `Obarray` — prefer method additions over open-coded unsafe dereferences at every call site.

Possible helpers to add in `symbol.rs`:
```rust
impl LispSymbol {
    /// Returns `Some(value)` if redirect==Plainval and val.plain != UNBOUND.
    pub fn plain_value(&self) -> Option<Value> {
        if self.flags.redirect() != SymbolRedirect::Plainval { return None; }
        let v = unsafe { self.val.plain };
        (v != Value::UNBOUND).then_some(v)
    }
    /// Returns `Some(target)` if redirect==Varalias.
    pub fn alias_target(&self) -> Option<SymId> {
        if self.flags.redirect() != SymbolRedirect::Varalias { return None; }
        Some(unsafe { self.val.alias })
    }
}
```

These methods may already exist under different names — grep first.

- [ ] **Step 7.3: Run sweep**

```
cargo check -p neovm-core
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
```

Expected: clean, 6/6, no new failures.

- [ ] **Step 7.4: Verify the grep now returns zero non-test hits**

```
grep -rn 'SymbolValue::' neovm-core/src --include='*.rs' | grep -v 'symbol.rs\|symbol_test.rs\|pdump/\|_test\.rs'
```

Expected: zero hits. If anything remains, it's a call site this task missed — fix it before committing.

- [ ] **Step 7.5: Commit**

```
git add neovm-core/src/emacs_core
git commit -m "symbol: Phase G — migrate external readers off SymbolValue

The 4 remaining non-test, non-pdump reads of sym.value outside
symbol.rs now dispatch on flags.redirect() + val union via new
accessors on LispSymbol. Part of Phase 10 of the symbol-redirect
refactor."
```

---

## Task 8: Phase H — delete `SymbolValue` enum, `LispSymbol::value`, `special`, `constant` fields

**Rationale:** Nothing reads or writes the legacy field anymore. Delete it and the enum.

**Files:**
- Modify: `neovm-core/src/emacs_core/symbol.rs`
- Modify: `neovm-core/src/emacs_core/symbol_test.rs`

- [ ] **Step 8.1: Delete the `SymbolValue` enum**

In `neovm-core/src/emacs_core/symbol.rs`, delete lines defining `pub enum SymbolValue` (near 241-253) and its `impl Default for SymbolValue` (near 255-259). Also delete the `// Legacy value-cell enum` section header.

- [ ] **Step 8.2: Delete the `value` field from `LispSymbol`**

In the `pub struct LispSymbol` definition (near 275-306), delete:

```rust
/// LEGACY value cell — see [`SymbolValue`]. Kept in sync with
/// `flags + val` during Phase 1; will be removed in Phase 10.
pub value: SymbolValue,
```

- [ ] **Step 8.3: Delete `special` and `constant` legacy mirror fields**

Per the doc comments at lines 293-299, these are legacy mirrors of `flags.declared_special()` and `flags.trapped_write() == NoWrite`. Delete both fields from the struct definition.

Then update every reader/writer of `.special` and `.constant`:

```
grep -n 'sym\.special\|\.special = \|sym\.constant\|\.constant = \|slot\.special\|slot\.constant' neovm-core/src --include='*.rs'
```

For each:
- Read of `.special` → `.flags.declared_special()`.
- Write of `.special = true/false` → `.flags.set_declared_special(true/false)`.
- Read of `.constant` → `.flags.trapped_write() == SymbolTrappedWrite::NoWrite`.
- Write of `.constant = true` → `.flags.set_trapped_write(SymbolTrappedWrite::NoWrite)`.
- Write of `.constant = false` → `.flags.set_trapped_write(SymbolTrappedWrite::UntrappedWrite)`.

Known callers per earlier grep: `symbol.rs` lines 587, 588, 657, 658, 668, 669, 952, 1455, 1582, 1588, 1595, 1601, 1638, 1766; `symbol_test.rs:614`; `pdump/convert.rs:2406-2407` (handled in Task 9).

- [ ] **Step 8.4: Delete the `pub type SymbolData = LispSymbol;` type alias**

Near line 308. Its Task 1-7 residents have already been renamed to `LispSymbol` via compile errors.

Run:
```
grep -rn 'SymbolData\b' neovm-core/src --include='*.rs'
```

For each, substitute `LispSymbol`. Then delete the type alias.

- [ ] **Step 8.5: `cargo check`**

```
cargo check -p neovm-core
```

Expected: clean. Any compile error points to a missed call site; fix it in place.

- [ ] **Step 8.6: Run regression + full sweep**

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
```

Expected: 6/6 regression, no new failures.

- [ ] **Step 8.7: Commit**

```
git add neovm-core/src/emacs_core
git commit -m "symbol: Phase H — delete SymbolValue enum and legacy mirrors

Deletes:
- pub enum SymbolValue (13 lines)
- LispSymbol::value field
- LispSymbol::special and LispSymbol::constant legacy mirrors
- pub type SymbolData = LispSymbol; alias

LispSymbol's value-cell shape now matches GNU struct Lisp_Symbol::s.val
(lisp.h:810-816) bit-for-bit. Readers of .special / .constant migrated
to flags.declared_special() / flags.trapped_write(). Part of Phase 10
of the symbol-redirect refactor."
```

---

## Task 9: Phase I — pdump format v11 → v12

**Rationale:** The dumped `DumpSymbol` shape currently encodes the legacy `SymbolValue` variants. With the enum gone, the dump format must change. Per project memory S105, no backward compatibility with older dumps is needed — old dumps are regenerated by `cargo xtask`.

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs`
- Modify: `neovm-core/src/emacs_core/pdump/types.rs` (or wherever `DumpSymbol` is defined)
- Modify: `neovm-core/src/emacs_core/pdump/mod.rs` (version constant)
- Test: `neovm-core/src/emacs_core/pdump/pdump_test.rs`

- [ ] **Step 9.1: Locate the `DumpSymbol` type and version constant**

```
grep -rn 'DumpSymbol\b\|PDUMP_VERSION\|DUMP_FORMAT_VERSION' neovm-core/src/emacs_core/pdump --include='*.rs'
```

Note the version constant value (expected v11) and the serialized struct definition.

- [ ] **Step 9.2: Redefine the `DumpSymbol` value field**

Change the current value-carrying field to a new `DumpSymbolVal` enum:

```rust
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Clone, Debug)]
pub enum DumpSymbolVal {
    /// PLAINVAL: either a live value or UNBOUND sentinel.
    Plain(DumpValue),
    /// VARALIAS: target SymId.
    Alias(u32),
    /// LOCALIZED: the BLV defcell default and local_if_set flag.
    /// Rebuilt on load via make_symbol_localized + set_blv_local_if_set.
    Localized {
        default: DumpValue,
        local_if_set: bool,
    },
    /// FORWARDED: re-installed by name at load time via install_buffer_objfwd.
    Forwarded { fwd_name: String },
}
```

Update `DumpSymbol` to contain `DumpSymbolVal` instead of the old field(s). Also encode `flags: u8` (bit-packed redirect + trapped_write + interned + declared_special) instead of the separate `special`/`constant` booleans.

- [ ] **Step 9.3: Update `dump_symbol` / `load_symbol` in `convert.rs`**

The dumping side now reads `sym.flags` + `sym.val` via redirect dispatch and produces the appropriate `DumpSymbolVal` variant. The loading side does the reverse: reads redirect + payload, calls the right mutator (`set_symbol_value_id` for Plain, `make_symbol_alias` for Alias, `make_symbol_localized` + `set_blv_local_if_set` for Localized, `install_buffer_objfwd` looked up by `fwd_name` for Forwarded).

Concrete pattern for the load side:
```rust
match dumped.val {
    DumpSymbolVal::Plain(v) => {
        let value = dump_value_to_value(v, &load_ctx);
        if value != Value::UNBOUND {
            obarray.set_symbol_value_id(sym_id, value);
        }
    }
    DumpSymbolVal::Alias(target_raw) => {
        let target = SymId(target_raw);
        obarray.make_symbol_alias(sym_id, target);
    }
    DumpSymbolVal::Localized { default, local_if_set } => {
        let def = dump_value_to_value(default, &load_ctx);
        obarray.make_symbol_localized(sym_id, def);
        obarray.set_blv_local_if_set(sym_id, local_if_set);
    }
    DumpSymbolVal::Forwarded { fwd_name } => {
        // Look up the static forwarder by name and install it.
        // Mirror the bootstrap-time install logic.
        crate::emacs_core::buffer_vars::install_forwarder_by_name(
            obarray,
            &fwd_name,
            sym_id,
        );
    }
}
```

(The exact helper to look up forwarders by name may need to be written — see `buffer_vars.rs` for the initial install path.)

- [ ] **Step 9.4: Bump the version constant**

Change the version constant from v11 → v12 in whatever file defines it. Update any comment that mentions v11.

- [ ] **Step 9.5: Update pdump round-trip tests**

In `neovm-core/src/emacs_core/pdump/pdump_test.rs`, search for any `SymbolValue::` uses or hardcoded v11 expectations and update to the new shape.

- [ ] **Step 9.6: Regenerate pdump fixtures if any**

```
grep -rn 'bootstrap.neoc\|bootstrap.pdump' neovm-core --include='*.toml' --include='*.rs' | head -5
```

If the build has a pdump file checked in (or generated by `cargo xtask`), it needs regeneration. Follow the regeneration command documented in the project (likely `cargo xtask pdump` or similar — see `CLAUDE.md`).

- [ ] **Step 9.7: Run pdump tests**

```
cargo nextest run -p neovm-core --lib -E 'test(pdump)' --no-fail-fast 2>&1 | tail -6
```

Expected: no new failures relative to baseline. If the bootstrap dump test fails, regenerate the dump (Step 9.6).

- [ ] **Step 9.8: Full sweep**

```
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
```

Expected: no new failures.

- [ ] **Step 9.9: Commit**

```
git add neovm-core/src/emacs_core/pdump
git commit -m "pdump: Phase I — format v11→v12 for SymbolValue-free DumpSymbol

With SymbolValue gone, DumpSymbol encodes the redirect tag + val union
directly via a new DumpSymbolVal enum. Load path dispatches to the
correct mutator per redirect arm. Old v11 dumps are discarded; cargo
xtask regenerates. Part of Phase 10 of the symbol-redirect refactor."
```

---

## Post-plan validation

After Task 9 lands, run the same sweep one more time as a final check:

```
cargo nextest run -p neovm-core --lib -E 'test(symbol_redirect_regression)' --no-fail-fast
cargo nextest run -p neovm-core --lib --no-fail-fast 2>&1 | tail -6
cargo nextest run -p neomacs-tui-tests --no-fail-fast 2>&1 | tail -8
```

Expected:
- 6/6 regression tests pass.
- Baseline (~91) lib-test failures; no increase.
- All 11 TUI tests pass.

Also run the success-criteria greps from the spec:

```
grep -rn 'SymbolValue\b' neovm-core/src --include='*.rs'                    # should be 0
grep -rn 'sym\.value\b\|\.value = SymbolValue::' neovm-core/src --include='*.rs'   # should be 0
```

---

## What ships after this plan

This plan delivers **step 1 of 4** toward `LispSymbol` matching GNU's shape exactly. The three remaining specs to write after this lands:

1. **`docs/superpowers/specs/YYYY-MM-DD-symbol-plist-as-cons-list-design.md`** — replace `plist: FxHashMap<SymId, Value>` with `plist: Value` (a Lisp cons list `(prop val prop val …)`). Matches GNU `Lisp_Symbol::s.plist`. Medium-scoped.

2. **`docs/superpowers/specs/YYYY-MM-DD-symbol-function-as-qnil-design.md`** — change `function: Option<Value>` to `function: Value` with `Qnil` sentinel for unbound. Small-scoped; cosmetic.

3. **`docs/superpowers/specs/YYYY-MM-DD-symbol-id-as-pointer-design.md`** — replace `SymId(u32)` with `NonNull<LispSymbol>` or equivalent. Eliminates the obarray-index indirection in favor of GNU's direct pointer. Large-scoped — touches GC, pdump, bytecode constants, every name → symbol resolution site.

4. (Optional) **`docs/superpowers/specs/YYYY-MM-DD-first-class-obarray-design.md`** — make `Obarray` a first-class Lisp value with hash-bucket-chain storage, enabling user-space `(make-obarray)` / `(intern name OBARRAY)`. Very large; only needed if user-space obarrays are required.

Each is a separate brainstorm → spec → plan → implement cycle.

---

## Self-review

**Spec coverage check:**

| Spec section | Plan task |
|---|---|
| §1 `SymbolVal::default()` → UNBOUND + Plainval arm | Task 1 |
| §2 Plainval reader collapse | Task 1 |
| §3 vm.rs Pass 2 deletion (3 sites) | Task 2 |
| §4 eval.rs Pass 2 deletion | Task 3 |
| §5 `CustomManager::auto_buffer_local` removal | Task 4 |
| §6 GC trace via redirect | Task 5 |
| §7 `is_buffer_local_id` redirect-backed | Task 2 (inline) |
| §8 internal mutators stop writing `sym.value` | Task 6 |
| §9 external readers migrated | Task 7 |
| §10 pdump v11→v12 | Task 9 |
| §11 delete enum + field + legacy mirrors | Task 8 |
| Testing §1 plainval void | Task 0 test 1 |
| Testing §2 LOCALIZED isolation | Task 0 test 3 |
| Testing §3 GC across Plainval | Task 0 test 6 |
| Testing §4 VARALIAS chain | Task 0 test 4 |
| Testing §5 FORWARDED conditional slot | Task 0 test 5 |
| Testing §6 pdump round-trip | Task 9 existing tests |

No gaps.

**Placeholder scan:** No "TBD", "TODO", or "implement details" left. Each step has concrete code or commands.

**Type consistency:** `SymbolRedirect`, `SymbolVal`, `LispSymbol`, `Value::UNBOUND`, `SymId`, `SymbolTrappedWrite::NoWrite` used consistently. Helpers `plain_value`, `alias_target` introduced in Task 7 are referenced in no earlier task (forward-only, so OK).
