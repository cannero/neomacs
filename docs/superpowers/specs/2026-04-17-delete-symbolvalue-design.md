# Delete Legacy `SymbolValue` Enum — Design

**Date:** 2026-04-17
**Status:** Approved
**Scope:** `neovm-core/src/emacs_core/symbol.rs` (primary), plus call-site updates in `bytecode/vm.rs`, `eval.rs`, `custom.rs`, `pdump/convert.rs`, and scattered test files.

## Motivation

NeoMacs's `LispSymbol` struct carries the symbol's value in **two parallel fields**:

- `val: SymbolVal` — a GNU-matching union discriminated by `flags.redirect()`.
- `value: SymbolValue` — a legacy enum that predates the symbol-redirect refactor.

Both fields are kept in sync by hand. This is the deferred second half of Phase 10 of the symbol-redirect refactor documented in `project_symbol_redirect_refactor.md` — "will be removed in Phase 10" comments on the legacy field have been present in the code for 9 days.

The parallel storage imposes four concrete costs:

1. **Performance.** `vm.rs::lookup_var_id` runs three sequential lookup passes. Pass 2 is a legacy-field gate that wraps the same modern data Pass 1 already consulted, plus a `SymId → String → intern(String) → SymId` round-trip and a potential `RwLock` acquisition on the global name registry. GNU's equivalent is one switch + one dereference. Every `Op::VarRef` in bytecode pays this cost.
2. **GC soundness.** `Obarray::trace_roots` walks `sym.value` (legacy), not `sym.val.plain` (new). Today that's safe because every mutator dual-writes. A single mutator that sets `sym.val.plain` without also updating `sym.value` would create a silent use-after-free.
3. **Silent drift.** The two fields are authoritative for *different* predicates. `is_buffer_local_id` reads legacy; `is_alias_id` reads new. Maintained by discipline, not by the type system.
4. **Memory.** Every interned symbol carries ~16-24 extra bytes for the legacy enum. With ~10-20K symbols in a running session, hundreds of KB of pure overhead.

## Goals

- Delete `SymbolValue` enum and `LispSymbol::value` field.
- Make `LispSymbol`'s value-cell shape bit-for-bit match GNU's `struct Lisp_Symbol::s.val` (`lisp.h:810-816`).
- Collapse `lookup_var_id` / `eval_symbol_by_id` from 3 passes to 1.
- GC traces walk the single authoritative storage (`sym.val`).
- Every read of symbol value state goes through `flags.redirect()` + `sym.val` dispatch. No reader consults a legacy marker.

## Non-goals

- Changing `plist: FxHashMap<SymId, Value>` → cons list (separate refactor; Option 2 in the scoping discussion).
- Changing `function: Option<Value>` → `Value` with `Qnil` sentinel (separate refactor).
- Changing `name: NameId` → `Lisp_Object` (separate refactor).
- Changing `SymId(u32)` → `*const LispSymbol` (large separate refactor; Option 3).
- Making obarrays first-class Lisp values (Option 4).
- Changes to `BufferLocals` (already deleted in Phase 10F).

## Architecture

### Before

```
LispSymbol {
    name: NameId,
    flags: SymbolFlags,     // { redirect, trapped_write, interned, declared_special, ... }
    val: SymbolVal,         // union: plain | alias | blv | fwd  ← NEW (GNU-shape)
    value: SymbolValue,     // enum: Plain(Option<Value>) | Alias | BufferLocal | Forwarded ← LEGACY
    function: Option<Value>,
    plist: FxHashMap<SymId, Value>,
    // ...
}

// vm.rs::lookup_var_id
Pass 1: flags.redirect() → Localized/Forwarded → find_symbol_value_in_buffer    [via val.blv or slot]
Pass 2: is_buffer_local_id(id) | is_auto_buffer_local_symbol(id)                [reads legacy field]
          → resolve_sym(id) → buf.get_buffer_local_binding(&str)                [redundant data path]
Pass 3: find_symbol_value(id) → match sym.value                                 [reads legacy field]

// Obarray::trace_roots
match &sym.value { Plain(Some(v)) => roots.push(v), ... }                        [legacy only]
```

### After

```
LispSymbol {
    name: NameId,
    flags: SymbolFlags,
    val: SymbolVal,         // sole value-cell storage (GNU-shape)
    function: Option<Value>,
    plist: FxHashMap<SymId, Value>,
    // ...
}

// vm.rs::lookup_var_id
Pass 1: flags.redirect() → all four arms dispatch through val                    [single switch]

// Obarray::trace_roots
match sym.flags.redirect() {
    Plainval => if val.plain != UNBOUND { roots.push(val.plain) }
    Varalias | Forwarded => {}       // alias chain / forwarder static data
    Localized => {}                  // traced separately via self.blvs
}
```

## Changes in detail

### 1. `symbol.rs` — `SymbolVal::default()` uses `UNBOUND`, not `NIL`

The legacy `SymbolValue::Plain(None)` represents "unbound". The new `val.plain` currently initializes to `Value::NIL` — indistinguishable from "bound to nil". Switch to the existing `Value::UNBOUND` sentinel (tag `111`):

```rust
impl Default for SymbolVal {
    fn default() -> Self {
        Self { plain: Value::UNBOUND }
    }
}
```

Every place that used `SymbolValue::Plain(None)` as "unbound" now uses `val.plain = Value::UNBOUND`.

### 2. `symbol.rs::find_symbol_value` — Plainval arm reads `val.plain`

Replace the Plainval arm at lines 982-990:

```rust
SymbolRedirect::Plainval => {
    let v = unsafe { sym.val.plain };
    if v == Value::UNBOUND { return None; } else { return Some(v); }
}
```

The `Varalias`, `Localized`, `Forwarded` arms already read from `val`; no change.

### 3. `vm.rs` — delete all three `is_buffer_local_id || is_auto_buffer_local_symbol` sites

Post-rebase line numbers:

- `lookup_var_id` around line 1982: delete the Pass 2 fallback block.
- `assign_var_id` around line 2154: same pattern, delete.
- `set-default` builtin around line 2389: the boolean `is_buffer_local` decides between a PLAINVAL write and a LOCALIZED write. Replace the legacy-marker check with `obarray.get_by_id(resolved).map(|s| s.flags.redirect() == SymbolRedirect::Localized).unwrap_or(false)`.

Comment at `lookup_var_id` (around line 1902) explains why Pass 2 exists; with the refactor complete, every LOCALIZED symbol is caught by Pass 1, and the fallback becomes dead code.

### 4. `eval.rs::eval_symbol_by_id` — delete Pass 2

Mirrors Phase 3 in the non-VM eval path. Remove the `get_buffer_local_binding_by_sym_id` fallback that follows the `read_localized` call.

Same surgery in `eval.rs::set_runtime_binding`.

### 5. `custom.rs` — remove `auto_buffer_local` mirror

- Delete `CustomManager::auto_buffer_local: FxHashSet<SymId>` field.
- Delete `make_variable_buffer_local_symbol`, `is_auto_buffer_local`, `is_auto_buffer_local_symbol` methods.
- Delete the `custom.make_variable_buffer_local_symbol(resolved_id)` line at `custom.rs:131`.
- Delete the `obarray.make_buffer_local(&resolved, true)` line at `custom.rs:130`.

### 6. `symbol.rs::trace_roots` — walk `val.plain` via redirect dispatch

```rust
fn trace_roots(&self, roots: &mut Vec<Value>) {
    for sym in self.symbols.iter().flatten() {
        match sym.flags.redirect() {
            SymbolRedirect::Plainval => {
                let v = unsafe { sym.val.plain };
                if v != Value::UNBOUND { roots.push(v); }
            }
            SymbolRedirect::Varalias | SymbolRedirect::Forwarded => {}
            SymbolRedirect::Localized => {}  // BLV traced below
        }
        if let Some(f) = sym.function { roots.push(f); }
        for pval in sym.plist.values() { roots.push(*pval); }
    }
    for &blv_ptr in &self.blvs {
        let blv = unsafe { &*blv_ptr };
        roots.push(blv.defcell);
        roots.push(blv.valcell);
        roots.push(blv.where_buf);
    }
}
```

**Critical safety invariant:** this change is only safe after steps 1-5 land. If `val.plain` isn't authoritative yet, swapping the trace source can free live values.

### 7. `symbol.rs::is_buffer_local_id` — check redirect tag, not legacy enum

```rust
pub fn is_buffer_local_id(&self, id: SymId) -> bool {
    self.slot(id).is_some_and(|s| s.flags.redirect() == SymbolRedirect::Localized)
}
```

Even though all callers inside the VM/eval Pass 2 are gone, the method is used by `(local-variable-p)` and similar builtins — keep it as a public API shape, just change the backing storage.

### 8. `symbol.rs` internal mutators — stop writing `sym.value`

Remove all `sym.value = SymbolValue::...` assignments from:

- `bootstrap_nil` / `bootstrap_t` — just initialize `val.plain`
- `define_keyword` — use `val.plain`
- `ensure_slot` — initial `val.plain = UNBOUND`
- `make_symbol_localized` — sets `val.blv` and `flags.redirect(Localized)`; no legacy-field write
- `install_buffer_objfwd` — sets `val.fwd` and `flags.redirect(Forwarded)`; no legacy-field write
- `set_symbol_value_id_inner` — writes `val.plain` based on current redirect arm
- `make_buffer_local` — delete this method entirely (last caller removed in step 5)

### 9. Call-site updates outside `symbol.rs`

Per grep, only 4 non-test sites reference `SymbolValue::` variants:

- `eval.rs` (1) — update to use redirect-tag dispatch
- `builtins/misc_eval.rs` (1) — same
- `interactive_test.rs` (1) — test; update to new API
- `builtins/tests.rs` (1) — test; update to new API

Plus 15 references in `pdump/convert.rs` (step 10).

### 10. `pdump/convert.rs` — new serialization shape

The dumped `DumpSymbol` currently encodes the legacy `SymbolValue` variants. Change to encode `redirect` tag + `val` payload. Bump dump format version v11 → v12. Old dumps are discarded (per memory S105 — no backward compat needed).

New serialized shape (rkyv):

```rust
struct DumpSymbol {
    name: u32,
    flags: u8,                      // bit-packed: redirect(2) + trapped(2) + interned(2) + declared(1)
    val: DumpSymbolVal,
    function: Option<DumpValue>,
    plist: Vec<(u32, DumpValue)>,
    // ...
}

enum DumpSymbolVal {
    Plain(DumpValue),               // UNBOUND-safe
    Alias(u32),
    Localized { default: DumpValue, local_if_set: bool },  // BLV rebuilt on load
    Forwarded { fwd_name: String }, // looked up at load time
}
```

### 11. Delete the enum and the field

After steps 1-10:

- `symbol.rs:241-253` — delete `enum SymbolValue` (13 lines).
- `symbol.rs:286` — delete `pub value: SymbolValue,` field.
- Remove `special: bool` and `constant: bool` fields (they're already legacy mirrors of `flags.declared_special()` and `flags.trapped_write() == NoWrite` per comments at lines 293-299).
- Run `cargo check` — should be clean.

## Data flow

```
read `x`:
  vm.rs::lookup_var_id(id)
  ├── lexenv lookup
  ├── resolve alias via val.alias
  └── obarray.find_symbol_value_in_buffer(id, current_buf)
      └── match sym.flags.redirect()
          ├── Plainval    → sym.val.plain
          ├── Varalias    → recurse on sym.val.alias
          ├── Localized   → swap BLV via sym.val.blv; read valcell
          └── Forwarded   → read buffer slot via sym.val.fwd

write `x`:
  vm.rs::assign_var_id(id, value)
  └── match sym.flags.redirect()
      ├── Plainval    → sym.val.plain = value
      ├── Varalias    → recurse on sym.val.alias
      ├── Localized   → set_internal_localized via BLV
      └── Forwarded   → write buffer slot via sym.val.fwd
```

## Testing

Correctness tests to write **before** Phase A lands:

1. **Plainval void semantics.** `(makunbound 'foo)` + `(symbol-value 'foo)` signals `void-variable`. `(set 'foo nil)` + `(symbol-value 'foo)` returns `nil`.
2. **Cross-buffer LOCALIZED isolation.** `(progn (setq vm-mlv-preserve-global 1) (with-temp-buffer (set (make-local-variable 'vm-mlv-preserve-global) 9) (list vm-mlv-preserve-global (default-value 'vm-mlv-preserve-global))))` returns `(9 1)` (matches GNU Emacs 31 as documented in `project_symbol_redirect_refactor.md`).
3. **GC across Plainval.** Set a symbol to a freshly-consed structure, force GC, read it back. Value must survive.
4. **VARALIAS chain.** `(defvaralias 'a 'b)`, `(defvaralias 'b 'c)`, set `a`, read `c` — must see the write.
5. **FORWARDED conditional slot.** `setq-default`, `make-local-variable`, `kill-local-variable`, `local-variable-p` on `fill-column` all GNU-identical.
6. **Pdump round-trip.** Dump + load preserves symbol values of every redirect shape.

These tests may already exist — verify and supplement as needed.

## Migration plan

Seven phases, each compile-clean and test-green. Each ships as a separate commit.

| Phase | Change | Risk |
|---|---|---|
| A | `SymbolVal::default()` → UNBOUND; `find_symbol_value::Plainval` reads `val.plain` | Low |
| B | Delete Pass 2 in `vm.rs::lookup_var_id` + `assign_var_id` | Medium — LOCALIZED test gate |
| C | Delete Pass 2 in `eval.rs::eval_symbol_by_id` + `set_runtime_binding` | Medium — same gate |
| D | Delete `CustomManager::auto_buffer_local` + call sites | Low |
| E | `Obarray::trace_roots` walks `val.plain` via redirect | **High — GC soundness** |
| F | Remove `sym.value` writes inside `symbol.rs` internals | Low after E |
| G | Remove `sym.value` reads outside `symbol.rs` (4 scattered sites) | Low |
| H | Delete `SymbolValue` enum + `LispSymbol::value` field + `special`/`constant` legacy fields | Trivial after F+G |
| I | pdump format v11 → v12 with new DumpSymbol shape | Medium — pdump round-trip tests |

Phases A–H are one sequential PR chain. Phase I can be bundled or deferred.

## Risks

1. **GC soundness during Phase E.** The most dangerous step. Writing a test that freshly allocates a cons into a Plainval symbol's value, forces GC, and verifies the cons survives must pass **before** we swap the trace source. If it fails, Phase A-D didn't fully migrate writes to `val.plain`.

2. **Phase B/C regressions on LOCALIZED symbols.** Some test may exercise a code path that creates a symbol via `make-variable-buffer-local` but, through some historical bootstrap quirk, ends up without the `Localized` redirect. Mitigation: the `let_buffer_local_does_not_corrupt_other_buffers` test from the memory is the gate. Baseline full suite is ~91 failing lib tests today; regression means any *new* failure.

3. **Phase I pdump version bump.** Old dumps become unloadable. Per memory S105, this is fine — no backward compat needed. But CI or local developer setups may expect a regenerable dump; verify `cargo xtask` can regenerate.

4. **Hidden legacy reads via `runtime_startup_state`.** The bootstrap/runtime-startup pipeline serializes symbol state during Lisp init. If any part of that path consults `sym.value` directly, Phase G may miss it. Mitigation: full grep for `.value` on symbol expressions as part of Phase G checklist.

## Success criteria

- `grep -rn 'SymbolValue\b' neovm-core/src` returns zero matches (outside comments or doc).
- `grep -rn 'sym\.value\b' neovm-core/src` returns zero matches.
- `LispSymbol` struct's `value` field is absent.
- `cargo check -p neovm-core` clean.
- Full buffer-module + symbol-module test sweep: no new failures vs. today's baseline (91 failing).
- Pdump round-trip test passes.
- `vm.rs::lookup_var_id` is visibly one switch on `redirect`, no legacy-gate fallback.
