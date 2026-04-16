# Unified Specpdl Design Spec

## Goal

Unify neomacs's three split runtime stacks (`Context.specpdl`, VM's `VmUnwindEntry`, `runtime_backtrace_frames`) into a single `specpdl` stack matching GNU Emacs's architecture. All entry types — let bindings, unwind-protect, save-excursion, save-restriction, backtrace frames — live on one stack. `unbind_to` handles everything in one pass, in correct reverse order.

This also fixes correctness bugs: the interpreter's `save-excursion` and `unwind-protect` currently don't restore/cleanup on nonlocal exit.

## Background

### GNU Emacs Design

GNU Emacs has one `specpdl` array (`union specbinding`) for all runtime state:

| Tag | Purpose |
|-----|---------|
| `SPECPDL_LET` | Dynamic variable binding |
| `SPECPDL_LET_LOCAL` | Buffer-local binding |
| `SPECPDL_LET_DEFAULT` | Default-value binding |
| `SPECPDL_UNWIND` | unwind-protect cleanup (function pointer + arg) |
| `SPECPDL_UNWIND_EXCURSION` | save-excursion state |
| `SPECPDL_BACKTRACE` | Call frame for backtrace |
| `SPECPDL_NOP` | Placeholder/cleared entry |

`unbind_to(count)` walks backward from the current position to `count`, calling `do_one_unbind` for each entry. This handles bindings, cleanups, excursions, and skips backtraces — all in one loop, in correct interleaved order.

The condition system (`handlerlist`) is separate from specpdl. The bytecode execution stack (`bc_frame`) is also separate. Both of these stay separate in neomacs too.

### Current Neomacs Design (Being Replaced)

Three separate stacks:

1. **`Context.specpdl: Vec<SpecBinding>`** — only bindings (`Let`, `LetLocal`, `LetDefault`, `LexicalEnv`) and GC roots (`GcRoot`)
2. **VM-local `specpdl: Vec<VmUnwindEntry>`** — unwind-protect (`Cleanup`), save-excursion (`Excursion`), save-restriction (`Restriction`), dynamic binding bookmarks (`DynamicBinding { specpdl_count }`)
3. **`Context.runtime_backtrace: Vec<RuntimeBacktraceFrame>`** — function + args index, with args stored in `Context.active_call_roots: Vec<ActiveCallFrame>`

Problems:
- Interleaving order between stacks is not preserved
- Interpreter's `unwind-protect` doesn't run cleanup on nonlocal exit
- Interpreter's `save-excursion` doesn't restore on nonlocal exit
- Two different unwind-protect implementations (interpreter vs VM)
- Backtrace builtins walk a separate structure instead of specpdl

---

## Design

### New `SpecBinding` Enum

```rust
pub(crate) enum SpecBinding {
    // === Existing (unchanged) ===
    Let { sym_id: SymId, old_value: Option<Value> },
    LetLocal { sym_id: SymId, old_value: Value, buffer_id: BufferId },
    LetDefault { sym_id: SymId, old_value: Option<Value> },
    LexicalEnv { old_lexenv: Value },
    GcRoot { value: Value },

    // === New: matches GNU SPECPDL_BACKTRACE ===
    Backtrace {
        function: Value,
        args: LispArgVec,
        debug_on_exit: bool,
    },

    // === New: matches GNU SPECPDL_UNWIND ===
    UnwindProtect { forms: Value, lexenv: Value },

    // === New: matches GNU SPECPDL_UNWIND_EXCURSION ===
    SaveExcursion { buffer_id: BufferId, marker_id: u64 },

    // === New: matches GNU SPECPDL_UNWIND with save_restriction_restore ===
    SaveRestriction { state: SavedRestrictionState },

    // === New: matches GNU SPECPDL_NOP ===
    Nop,
}
```

GNU uses `SPECPDL_UNWIND` with a C function pointer for both unwind-protect cleanups and save-restriction restore. In Rust, we use dedicated variants since we can't store arbitrary function pointers with the same pattern. Same semantics, type-safe.

### Unified `unbind_to`

One function handles all entry types:

```rust
pub(crate) fn unbind_to(&mut self, count: usize) {
    while self.specpdl.len() > count {
        let binding = self.specpdl.pop().unwrap();
        match binding {
            SpecBinding::Let { sym_id, old_value } => {
                // Restore old value in obarray (existing logic)
            }
            SpecBinding::LetLocal { sym_id, old_value, buffer_id } => {
                // Restore buffer-local value (existing logic)
            }
            SpecBinding::LetDefault { sym_id, old_value } => {
                // Restore default value (existing logic)
            }
            SpecBinding::LexicalEnv { old_lexenv } => {
                self.lexenv = old_lexenv;
            }
            SpecBinding::GcRoot { .. } => {
                // No-op, discard
            }
            SpecBinding::Backtrace { .. } => {
                // No-op, discard (matches GNU)
            }
            SpecBinding::Nop => {
                // No-op (matches GNU)
            }
            SpecBinding::UnwindProtect { forms, lexenv } => {
                // Evaluate cleanup forms in the saved lexical environment.
                // Entry is already popped, so re-entrant errors don't
                // re-unwind this entry (matches GNU's pre-decrement).
                let saved_lexenv = self.lexenv;
                self.lexenv = lexenv;
                let _ = self.sf_progn_value(forms);
                self.lexenv = saved_lexenv;
            }
            SpecBinding::SaveExcursion { buffer_id, marker_id } => {
                // Restore buffer and point (matches GNU save_excursion_restore).
                self.restore_current_buffer_if_live(buffer_id);
                if let Some(saved_pt) = self.buffers.marker_position(buffer_id, marker_id) {
                    let _ = self.buffers.goto_buffer_byte(buffer_id, saved_pt);
                }
                self.buffers.remove_marker(marker_id);
            }
            SpecBinding::SaveRestriction { state } => {
                self.buffers.restore_saved_restriction_state(state);
            }
        }
    }
}
```

### Interpreter Special Forms

All three follow the same pattern: push entry → eval body → unbind_to.

**unwind-protect:**
```rust
fn sf_unwind_protect_value(&mut self, tail: Value) -> EvalResult {
    let body = tail.cons_car();
    let cleanup_forms = tail.cons_cdr();
    let count = self.specpdl.len();
    self.specpdl.push(SpecBinding::UnwindProtect {
        forms: cleanup_forms,
        lexenv: self.lexenv,
    });
    let result = self.eval_sub(body);
    self.unbind_to(count);  // cleanup runs on ALL exits
    result
}
```

**save-excursion:**
```rust
fn sf_save_excursion_value(&mut self, tail: Value) -> EvalResult {
    let count = self.specpdl.len();
    if let Some(buf_id) = self.buffers.current_buffer_id() {
        let pt = self.buffers.get(buf_id).map(|b| b.pt_byte).unwrap_or(0);
        let marker_id = self.buffers.create_marker(buf_id, pt, InsertionType::Before);
        self.specpdl.push(SpecBinding::SaveExcursion { buffer_id: buf_id, marker_id });
    }
    let result = self.sf_progn_value(tail);
    self.unbind_to(count);  // restores on ALL exits
    result
}
```

**save-restriction:**
```rust
fn sf_save_restriction_value(&mut self, tail: Value) -> EvalResult {
    let count = self.specpdl.len();
    if let Some(state) = self.buffers.save_current_restriction_state() {
        self.specpdl.push(SpecBinding::SaveRestriction { state });
    }
    let result = self.sf_progn_value(tail);
    self.unbind_to(count);
    result
}
```

### VM Changes

**Delete `VmUnwindEntry` entirely.** The VM pushes directly onto `Context.specpdl`.

**Frame setup/teardown:**
```rust
// At frame entry:
let specpdl_base = self.ctx.specpdl.len();

// At normal frame exit:
self.ctx.unbind_to(specpdl_base);
```

Matches GNU's `exec_byte_code` which saves `count = SPECPDL_INDEX()` and calls `unbind_to(count, result)`.

**Opcode changes:**

| Opcode | Before | After |
|--------|--------|-------|
| `VarBind` | `ctx.specbind()` + push `VmUnwindEntry::DynamicBinding` | `ctx.specbind()` only |
| `Bunbind(n)` | Pop n `VmUnwindEntry`, restore each | `ctx.unbind_to(saved_depth)` where saved_depth was captured before the n bindings |
| `UnwindProtectPop` | Push `VmUnwindEntry::Cleanup` | Push `SpecBinding::UnwindProtect` onto `ctx.specpdl` |
| `Bsave_excursion` | Push `VmUnwindEntry::Excursion` | Push `SpecBinding::SaveExcursion` onto `ctx.specpdl` |
| `Bsave_restriction` | Push `VmUnwindEntry::Restriction` | Push `SpecBinding::SaveRestriction` onto `ctx.specpdl` |

**`Bunbind(n)` tracking:** The VM needs to know the specpdl depth to unwind to. GNU's `exec_byte_code` maintains this implicitly — each `specbind` increments `specpdl_ptr`, and `Bunbind(n)` subtracts n. With the unified specpdl containing mixed entry types, the VM must track depth explicitly. Two approaches:

1. **Save specpdl depth before each binding group** in a local depth stack. `Bunbind(n)` pops n depths and calls `unbind_to(earliest)`.
2. **Count backwards** from current specpdl length, skipping non-binding entries.

Option 1 is cleaner and matches how GNU works (specpdl_ptr arithmetic is equivalent to tracking depths).

**Nonlocal exit (`resume_nonlocal`):** The `ResumeTarget` already stores `spec_depth`. On catch/condition-case resume, call `ctx.unbind_to(spec_depth)`. This single call unwinds all bindings, runs all unwind-protect cleanups, restores all excursions/restrictions — in correct reverse order. The current manual `VmUnwindEntry` walk is deleted.

### Backtrace on Specpdl

**Push:** At function entry (both interpreter and VM), push `SpecBinding::Backtrace { function, args, debug_on_exit: false }` onto specpdl.

**Pop:** `unbind_to` discards `Backtrace` entries (no-op), same as GNU.

**Walking:** Backtrace builtins walk specpdl backward filtering for `Backtrace` entries:

```rust
pub fn backtrace_frames(&self) -> impl Iterator<Item = &SpecBinding> + '_ {
    self.specpdl.iter().rev().filter(|b| matches!(b, SpecBinding::Backtrace { .. }))
}
```

### Deleted Structures

- `enum VmUnwindEntry` (vm.rs)
- `struct RuntimeBacktraceFrame` (eval.rs)
- `struct ActiveCallFrame` (eval.rs)
- `runtime_backtrace: Vec<RuntimeBacktraceFrame>` field on Context
- `active_call_roots: Vec<ActiveCallFrame>` field on Context
- All push/pop methods for the above
- VM's `collect_specpdl_roots`, `restore_unwind_entry`, `unwind_specpdl_*`

### GC Tracing

`trace_roots` walks `specpdl` once, tracing all variants:

| Variant | Values to trace |
|---------|----------------|
| `Let` | `old_value` |
| `LetLocal` | `old_value` |
| `LetDefault` | `old_value` |
| `LexicalEnv` | `old_lexenv` |
| `GcRoot` | `value` |
| `Backtrace` | `function`, each element of `args` |
| `UnwindProtect` | `forms`, `lexenv` |
| `SaveExcursion` | (no Values — buffer_id and marker_id are not GC-managed) |
| `SaveRestriction` | `state.trace_roots()` (already implemented) |
| `Nop` | (nothing) |

Tracing for `active_call_roots` and `runtime_backtrace` is deleted.

### Not Changed

- `condition_stack: Vec<ConditionFrame>` — stays separate (matches GNU's `handlerlist`)
- `bc_buf` / `bc_frames` — bytecode execution stack stays separate
- `SpecBinding::GcRoot` — stays (neomacs-specific, needed for precise GC)
