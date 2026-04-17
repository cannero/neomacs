# `LispSymbol::function` as `Value` with Qnil Sentinel — Design

**Date:** 2026-04-17
**Status:** Approved
**Scope:** `neovm-core/src/emacs_core/symbol.rs` (primary), plus callers in `eval.rs`, `pdump/convert.rs`, `pdump/types.rs`, and any test files that inspect the field.

## Motivation

`LispSymbol::function` is currently `Option<Value>`. GNU stores it as `Lisp_Object function` — a single tagged value where `Qnil` means "unbound function" (`lisp.h:820`; `data.c::Ffboundp`; `data.c::Ffmakunbound`). The `Option` wrapper encodes no information GNU doesn't encode: GNU's `Qnil` and NeoMacs's `None` represent the same state. Today NeoMacs must carry an `Option` tag byte per symbol, disambiguate `None` vs `Some(Value::NIL)` at every reader (even though both mean "unbound"), and wrap/unwrap at every boundary.

This refactor replaces `function: Option<Value>` with `function: Value`, matching GNU's layout exactly. `Value::NIL` is the unbound sentinel.

## Goals

- `LispSymbol::function: Value`, replacing `Option<Value>`.
- `fboundp_id(id)` returns true iff `!sym.function.is_nil()`.
- `fmakunbound_id(id)` writes `Value::NIL`.
- `fset`-style writes store the incoming `Value` directly.
- GC trace pushes `sym.function` unconditionally (NIL is immediate, harmless to push).
- Pdump `DumpSymbolData::function` matches — no `Option`.

## Non-goals

- Changing any other field of `LispSymbol` (plist already done; other follow-ups tracked separately).
- Semantic behavior change for `fboundp` / `fmakunbound` / `fset` / `symbol-function`. GNU treats `Qnil` as "no function"; NeoMacs already does. After this refactor, `(fset 'foo nil)` + `(fboundp 'foo)` returns `nil` — same as today, same as GNU.
- Performance improvements beyond the trivial 1-byte-per-symbol saving.

## Architecture

### Before

```rust
pub struct LispSymbol {
    // ...
    pub function: Option<Value>,   // None = unbound, Some(Value::NIL) = also unbound
    // ...
}
```

Reader pattern (repeated at ~10 sites):
```rust
if let Some(f) = sym.function { roots.push(f); }
```

### After

```rust
pub struct LispSymbol {
    // ...
    pub function: Value,           // Value::NIL = unbound; any other value = bound
    // ...
}
```

Reader pattern:
```rust
roots.push(sym.function);    // NIL pushes are harmless (tag=000, immediate)
```

Or with explicit "bound?" check:
```rust
if !sym.function.is_nil() { /* use sym.function */ }
```

## Changes in detail

### 1. Struct field change (`symbol.rs`)

```rust
-    pub function: Option<Value>,
+    pub function: Value,
```

All constructor / `Default` initializers for `LispSymbol`:
```rust
-    function: None,
+    function: Value::NIL,
```

### 2. Accessor methods (`symbol.rs`)

**`fboundp_id`** (around line 1475):
```rust
pub fn fboundp_id(&self, id: SymId) -> bool {
    self.slot(id)
        .is_some_and(|s| !s.function.is_nil() && !s.function_unbound)
}
```

(Keep the `function_unbound` flag semantics — per project memory it exists for an `fmakunbound` edge case.)

**`fmakunbound_id`** (around line 1381):
```rust
pub fn fmakunbound_id(&mut self, id: SymId) {
    if let Some(sym) = self.slot_mut(id) {
        sym.function = Value::NIL;
        sym.function_unbound = true;
    }
}
```

**`symbol_function_id`** / similar getters:
```rust
// Before: fn symbol_function(&self, id: SymId) -> Option<Value>
// After:  fn symbol_function(&self, id: SymId) -> Value

pub fn symbol_function_id(&self, id: SymId) -> Value {
    self.slot(id).map(|s| s.function).unwrap_or(Value::NIL)
}
```

Callers that did `.unwrap_or(Value::NIL)` drop the call — the return is already the right shape.

**`fset_id` / `set_function`** writers: take a `Value`, store directly:
```rust
pub fn set_function_id(&mut self, id: SymId, value: Value) {
    if let Some(sym) = self.slot_mut(id) {
        sym.function = value;
        sym.function_unbound = false;  // explicit set clears the unbound flag
    }
}
```

### 3. GC trace (`symbol.rs::Obarray::trace_roots`)

Current:
```rust
if let Some(f) = sym.function {
    roots.push(f);
}
```

Replace with:
```rust
roots.push(sym.function);
```

Pushing NIL is a no-op for the cons tracer (NIL is an immediate value with no payload).

### 4. Pdump (`pdump/types.rs` + `pdump/convert.rs`)

In `DumpSymbolData`:
```rust
-    pub function: Option<DumpValue>,
+    pub function: DumpValue,
```

In `dump_symbol_data`:
```rust
-    function: sym.function.map(|f| encoder.dump_value(f)),
+    function: encoder.dump_value(sym.function),
```

In `load_symbol_data`:
```rust
-    function: sd.function.as_ref().map(|f| decoder.load_value(f)),
+    function: decoder.load_value(&sd.function),
```

Bump dump format version 22 → 23.

### 5. External callers

Per grep: 1 site in `eval.rs`, 1 in `pdump/convert.rs`. Both do `sym.function.as_ref()` / similar. Each becomes a direct `sym.function` read, possibly with `!sym.function.is_nil()` if they check boundness.

## Testing

Regression test file: `neovm-core/src/emacs_core/symbol_function_regression_test.rs`. Four tests:

1. **`fboundp_unbound_returns_nil`** — `(fboundp 'unbound-foo)` returns `nil` on a fresh symbol.
2. **`fset_then_fboundp_returns_t`** — `(fset 'foo 'bar)` then `(fboundp 'foo)` returns `t`.
3. **`fmakunbound_resets_fboundp`** — `(fset 'foo 'bar)`, `(fmakunbound 'foo)`, `(fboundp 'foo)` returns `nil`.
4. **`fset_to_nil_is_unbound`** — `(fset 'foo nil)`, `(fboundp 'foo)` returns `nil` (GNU semantic: nil == unbound for the function slot).
5. **`symbol_function_survives_gc`** — `(fset 'gc-fn (lambda () 42))`, force GC, `(funcall 'gc-fn)` returns 42. Catches trace-roots migration correctness.

All five should pass today (the semantic is unchanged) and must continue to pass through the refactor.

## Migration plan

Three phases, each compile-clean + test-green.

### Phase A — Regression harness

Add the 5 tests above. All pass today. Gate for the subsequent migration.

### Phase B — Flip the field + migrate all readers + writers + GC trace

Single-commit change touching:
- `symbol.rs` struct, accessor methods, constructors, `trace_roots`.
- `eval.rs` caller.
- `pdump/convert.rs` + `pdump/types.rs` (non-version-bump parts).
- Tests that inspect the field directly.

All 5 regression tests must pass after the commit.

### Phase C — Pdump format v22 → v23

Bump the format version. Small commit.

## Risks

1. **`function_unbound` interaction.** The `function_unbound: bool` flag exists on `LispSymbol` for edge cases per project memory. The `fboundp` check must continue to honor it: `!function.is_nil() && !function_unbound`. Otherwise a call sequence `fset → fmakunbound` may report inconsistent fboundp. Existing behavior is preserved by the design above.

2. **GC push of NIL values.** Pushing `Value::NIL` to the GC roots vec is harmless but adds a tiny per-symbol cost (~one vec push per symbol per GC). For ~20K symbols that's 20K trivial pushes. Negligible. Alternative is to keep the `is_nil` check in `trace_roots`; not worth the branch.

3. **Pdump format bump invalidates dumps.** Standard — regenerate via `cargo xtask`.

## Success criteria

- `grep -rn 'function: Option<Value>' neovm-core/src` returns zero hits.
- `grep -rn 'sym\.function\.as_ref\|sym\.function\.map\|\.function = Some\|\.function = None' neovm-core/src` returns zero hits.
- 5/5 regression tests pass.
- Full buffer+symbol+eval+custom+bytecode sweep: no new failures vs. post-plist-refactor baseline.
