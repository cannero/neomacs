# Unified Specpdl Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify neomacs's three split runtime stacks into a single specpdl matching GNU Emacs, fixing unwind-protect/save-excursion correctness bugs.

**Architecture:** Extend `SpecBinding` with `Backtrace`, `UnwindProtect`, `SaveExcursion`, `SaveRestriction`, `Nop` variants. Update `unbind_to` to handle all types. Rewrite interpreter special forms and VM opcodes to push onto `Context.specpdl`. Delete `VmUnwindEntry`, `RuntimeBacktraceFrame`, `ActiveCallFrame`.

**Tech Stack:** Rust (neovm-core crate)

**Spec:** `docs/superpowers/specs/2026-04-15-unified-specpdl-design.md`

---

## File Map

| File | Role | Changes |
|------|------|---------|
| `neovm-core/src/emacs_core/eval.rs` | SpecBinding enum, unbind_to, trace_roots, backtrace/call frame infrastructure | Add variants, extend unbind_to, update trace_roots, delete old structures |
| `neovm-core/src/emacs_core/bytecode/vm.rs` | Bytecode VM interpreter | Delete VmUnwindEntry, rewrite opcodes to use Context.specpdl |
| `neovm-core/src/emacs_core/misc.rs` | Backtrace builtins | Rewrite to walk specpdl instead of runtime_backtrace |
| `neovm-core/src/emacs_core/eval_test.rs` | Eval tests | Update tests referencing old structures |
| `neovm-core/src/test_utils.rs` | Test helpers | Update if using active_call_roots |

---

### Task 1: Add new SpecBinding variants and update unbind_to

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs:149-178` (SpecBinding enum)
- Modify: `neovm-core/src/emacs_core/eval.rs:10324-10467` (unbind_to)
- Modify: `neovm-core/src/emacs_core/eval.rs:4309-4322` (trace_roots specpdl section)

This task adds the new variants but does NOT yet change any callers. Everything that currently pushes the old types still works. The new variants are simply available for subsequent tasks.

- [ ] **Step 1: Add new variants to SpecBinding**

In `neovm-core/src/emacs_core/eval.rs`, find the `SpecBinding` enum (line 149) and add after the `GcRoot` variant:

```rust
    /// Call frame for backtrace. Matches GNU SPECPDL_BACKTRACE.
    /// unbind_to discards these (no-op).
    Backtrace {
        function: Value,
        args: LispArgVec,
        debug_on_exit: bool,
    },
    /// unwind-protect cleanup forms + lexical environment.
    /// Matches GNU SPECPDL_UNWIND. unbind_to evaluates the forms.
    UnwindProtect { forms: Value, lexenv: Value },
    /// save-excursion state. Matches GNU SPECPDL_UNWIND_EXCURSION.
    /// unbind_to restores buffer and point.
    SaveExcursion { buffer_id: crate::buffer::BufferId, marker_id: u64 },
    /// save-current-buffer state. Matches GNU record_unwind_current_buffer.
    /// unbind_to restores the current buffer without restoring point.
    SaveCurrentBuffer { buffer_id: crate::buffer::BufferId },
    /// save-restriction state. Matches GNU SPECPDL_UNWIND with
    /// save_restriction_restore. unbind_to restores narrowing.
    SaveRestriction { state: crate::buffer::SavedRestrictionState },
    /// Placeholder. Matches GNU SPECPDL_NOP.
    Nop,
```

Note: Check that `SavedRestrictionState` is importable from `crate::buffer` — find its actual location with `grep -rn "struct SavedRestrictionState"` and use the correct path.

- [ ] **Step 2: Add new arms to unbind_to**

In the `unbind_to` method (line 10324), add match arms for the new variants after the existing `GcRoot` arm. The existing arms for `Let`, `LetLocal`, `LetDefault`, `LexicalEnv`, `GcRoot` remain unchanged.

```rust
                SpecBinding::Backtrace { .. } => {
                    // No-op: backtrace frames are informational only.
                    // Matches GNU do_one_unbind SPECPDL_BACKTRACE.
                }
                SpecBinding::Nop => {
                    // No-op. Matches GNU SPECPDL_NOP.
                }
                SpecBinding::UnwindProtect { forms, lexenv } => {
                    // Evaluate cleanup forms in the saved lexical environment.
                    // Entry is already popped so re-entrant errors during cleanup
                    // don't re-unwind this entry (matches GNU's pre-decrement of
                    // specpdl_ptr before calling do_one_unbind).
                    let saved_lexenv = self.lexenv;
                    self.lexenv = lexenv;
                    let _ = self.sf_progn_value(forms);
                    self.lexenv = saved_lexenv;
                }
                SpecBinding::SaveExcursion { buffer_id, marker_id } => {
                    // Restore buffer and point. Matches GNU save_excursion_restore.
                    self.restore_current_buffer_if_live(buffer_id);
                    if let Some(saved_pt) = self.buffers.marker_position(buffer_id, marker_id) {
                        let _ = self.buffers.goto_buffer_byte(buffer_id, saved_pt);
                    }
                    self.buffers.remove_marker(marker_id);
                }
                SpecBinding::SaveCurrentBuffer { buffer_id } => {
                    // Restore current buffer without restoring point.
                    // Matches GNU record_unwind_current_buffer / set_buffer_if_live.
                    self.restore_current_buffer_if_live(buffer_id);
                }
                SpecBinding::SaveRestriction { state } => {
                    self.buffers.restore_saved_restriction_state(state);
                }
```

- [ ] **Step 3: Update trace_roots for new variants**

In `trace_roots` (line 4309), update the specpdl loop to trace the new variants. Replace the existing match block:

```rust
        for entry in &self.specpdl {
            match entry {
                SpecBinding::Let {
                    old_value: Some(val),
                    ..
                } => visit(*val),
                SpecBinding::LetLocal { old_value, .. } => visit(*old_value),
                SpecBinding::LetDefault {
                    old_value: Some(val),
                    ..
                } => visit(*val),
                SpecBinding::LexicalEnv { old_lexenv } => visit(*old_lexenv),
                SpecBinding::GcRoot { value } => visit(*value),
                SpecBinding::Backtrace { function, args, .. } => {
                    visit(*function);
                    for arg in args.iter().copied() {
                        visit(arg);
                    }
                }
                SpecBinding::UnwindProtect { forms, lexenv } => {
                    visit(*forms);
                    visit(*lexenv);
                }
                SpecBinding::SaveRestriction { state } => {
                    state.trace_roots(&mut |v| visit(v));
                }
                _ => {}
            }
        }
```

Note: Check how `SavedRestrictionState::trace_roots` is actually called — look at `VmUnwindEntry::Restriction` tracing in vm.rs:190 for the correct invocation pattern and adapt.

- [ ] **Step 4: Also update unbind_to_in_state if needed**

The standalone `unbind_to_in_state` function (line 10494) is used by the VM. It may need the new arms too, or it can panic on the new variants since after the full migration it should never encounter them (the VM will use `ctx.unbind_to` directly). For now, add a catch-all: `_ => panic!("unexpected SpecBinding variant in unbind_to_in_state")`.

- [ ] **Step 5: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

Expected: compiles with warnings about unused variants (they have no pushers yet).

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/emacs_core/eval.rs
git commit -m "Add Backtrace, UnwindProtect, SaveExcursion, SaveRestriction, Nop to SpecBinding"
```

---

### Task 2: Rewrite interpreter unwind-protect

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs:7938-7990` (sf_unwind_protect_value / sf_unwind_protect_value_named)

- [ ] **Step 1: Rewrite sf_unwind_protect_value_named**

Find `sf_unwind_protect_value_named` (line 7942). Replace the entire function body with the specpdl-based pattern:

```rust
    fn sf_unwind_protect_value_named(&mut self, _call_name: &str, tail: Value) -> EvalResult {
        if tail.is_nil() {
            return Ok(Value::NIL);
        }
        let body = tail.cons_car();
        let cleanup_forms = tail.cons_cdr();
        let count = self.specpdl.len();
        self.specpdl.push(SpecBinding::UnwindProtect {
            forms: cleanup_forms,
            lexenv: self.lexenv,
        });
        let result = self.eval_sub(body);
        self.unbind_to(count);
        result
    }
```

This replaces the old "eval body, root result as GcRoot, eval cleanup, restore roots" pattern. Now `unbind_to(count)` runs the cleanup forms during unwind — on BOTH normal exit and nonlocal exit (signal/throw).

- [ ] **Step 2: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add neovm-core/src/emacs_core/eval.rs
git commit -m "Rewrite interpreter unwind-protect to use specpdl"
```

---

### Task 3: Rewrite interpreter save-excursion

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs:8120-8148` (sf_save_excursion_value)

- [ ] **Step 1: Rewrite sf_save_excursion_value**

Find `sf_save_excursion_value` (line 8120). Replace the entire function body:

```rust
    fn sf_save_excursion_value(&mut self, tail: Value) -> EvalResult {
        let count = self.specpdl.len();
        if let Some(buf_id) = self.buffers.current_buffer().map(|b| b.id) {
            let pt = self.buffers.get(buf_id).map(|b| b.pt_byte).unwrap_or(0);
            let marker_id = self.buffers.create_marker(
                buf_id,
                pt,
                crate::buffer::InsertionType::Before,
            );
            self.specpdl.push(SpecBinding::SaveExcursion {
                buffer_id: buf_id,
                marker_id,
            });
        }
        let result = self.sf_progn_value(tail);
        self.unbind_to(count);
        result
    }
```

This fixes the correctness bug: `unbind_to` now restores on ALL exits, not just normal exit.

- [ ] **Step 2: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add neovm-core/src/emacs_core/eval.rs
git commit -m "Rewrite interpreter save-excursion to use specpdl"
```

---

### Task 4: Rewrite interpreter save-restriction

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs:8151-8167` (sf_save_restriction_value)

- [ ] **Step 1: Rewrite sf_save_restriction_value**

Find `sf_save_restriction_value` (line 8151). Replace the entire function body:

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

- [ ] **Step 2: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add neovm-core/src/emacs_core/eval.rs
git commit -m "Rewrite interpreter save-restriction to use specpdl"
```

---

### Task 5: Rewrite VM opcodes to use Context.specpdl

**Files:**
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs`

This is the largest task. The VM's `run_loop` currently threads a `specpdl: &mut Vec<VmUnwindEntry>` parameter. After this task, that parameter is replaced with specpdl depth tracking via `Context.specpdl`.

- [ ] **Step 1: Remove the `specpdl` parameter from run_loop and run_frame**

In `run_frame` (line 267): remove `let mut specpdl: Vec<VmUnwindEntry> = Vec::new();` (line 281). Instead save: `let specpdl_base = self.ctx.specpdl.len();`.

In `run_loop` (line 468): remove the `specpdl: &mut Vec<VmUnwindEntry>` parameter. Replace all `specpdl` references inside with direct `self.ctx.specpdl` operations.

- [ ] **Step 2: Rewrite VarBind opcode (line 610-615)**

Before:
```rust
Op::VarBind(idx) => {
    let name_id = sym_id_at(constants, *idx);
    let val = stk!().pop().unwrap_or(Value::NIL);
    let specpdl_count = self.ctx.specpdl.len();
    self.ctx.specbind(name_id, val);
    specpdl.push(VmUnwindEntry::DynamicBinding { specpdl_count });
}
```

After:
```rust
Op::VarBind(idx) => {
    let name_id = sym_id_at(constants, *idx);
    let val = stk!().pop().unwrap_or(Value::NIL);
    self.ctx.specbind(name_id, val);
}
```

No VmUnwindEntry push needed — `specbind` already pushes onto `Context.specpdl`.

- [ ] **Step 3: Rewrite Unbind opcode (line 617-622)**

Before:
```rust
Op::Unbind(n) => {
    let mut unwind_roots = Vec::new();
    Self::collect_specpdl_roots(specpdl, &mut |value| unwind_roots.push(value));
    vm_try!(self.with_frame_roots(func, &[], &unwind_roots, |vm| {
        vm.unwind_specpdl_n(*n as usize, specpdl)
    },));
}
```

The VM needs to know the specpdl depth to unwind to. GNU Emacs's `Bunbind(n)` subtracts n from the current specpdl pointer. Since our specpdl has mixed entry types, the cleanest approach is tracking bind depths.

Add a `bind_stack: Vec<usize>` local to `run_loop` (or reuse the frame setup). Each time `VarBind` executes, push the current specpdl depth BEFORE the bind. On `Unbind(n)`, pop n entries from `bind_stack` to get the target depth:

```rust
// At frame start:
let mut bind_stack: Vec<usize> = Vec::new();

// VarBind:
Op::VarBind(idx) => {
    bind_stack.push(self.ctx.specpdl.len());
    let name_id = sym_id_at(constants, *idx);
    let val = stk!().pop().unwrap_or(Value::NIL);
    self.ctx.specbind(name_id, val);
}

// Unbind:
Op::Unbind(n) => {
    let n = *n as usize;
    if n > 0 {
        let target = bind_stack[bind_stack.len() - n];
        bind_stack.truncate(bind_stack.len() - n);
        self.ctx.unbind_to(target);
    }
}
```

Note: `unbind_to(target)` may call cleanup functions (for UnwindProtect entries between bindings). This is correct — it matches GNU where `Bunbind` unwinds everything between the current specpdl position and the target.

- [ ] **Step 4: Rewrite SaveExcursion opcode (line 772-787)**

Before:
```rust
Op::SaveExcursion => {
    if let Some((buffer_id, point)) = self.ctx.buffers
        .current_buffer().map(|buffer| (buffer.id, buffer.pt_byte))
    {
        let marker_id = self.ctx.buffers.create_marker(buffer_id, point, InsertionType::Before);
        specpdl.push(VmUnwindEntry::Excursion { buffer_id, marker_id });
    }
}
```

After:
```rust
Op::SaveExcursion => {
    if let Some((buffer_id, point)) = self.ctx.buffers
        .current_buffer().map(|buffer| (buffer.id, buffer.pt_byte))
    {
        let marker_id = self.ctx.buffers.create_marker(
            buffer_id, point, crate::buffer::InsertionType::Before,
        );
        self.ctx.specpdl.push(SpecBinding::SaveExcursion { buffer_id, marker_id });
    }
}
```

Also update the `bind_stack` to track this — push current specpdl len before pushing SaveExcursion, so `Unbind(n)` can undo it:

```rust
Op::SaveExcursion => {
    bind_stack.push(self.ctx.specpdl.len());
    // ... push SaveExcursion ...
}
```

Wait — this requires understanding how GNU handles `Bsave_excursion` + `Bunbind`. In GNU, `Bsave_excursion` pushes an unwind entry and `Bunbind_all` or the matching `Bunbind(n)` unwinds it. The n in `Bunbind(n)` counts the number of specpdl entries to undo — not just let-bindings but also excursions, restrictions, unwind-protects.

So `bind_stack` should track ALL specpdl pushes that correspond to `Bunbind`-able entries — not just VarBind. Rename it to `unwind_depth_stack` for clarity.

- [ ] **Step 5: Rewrite SaveRestriction opcode (line 789-792)**

```rust
Op::SaveRestriction => {
    unwind_depth_stack.push(self.ctx.specpdl.len());
    if let Some(saved) = self.ctx.buffers.save_current_restriction_state() {
        self.ctx.specpdl.push(SpecBinding::SaveRestriction { state: saved });
    }
}
```

- [ ] **Step 6: Rewrite SaveCurrentBuffer opcode (line 765-770)**

Currently pushes `VmUnwindEntry::CurrentBuffer`. There's no `SpecBinding::SaveCurrentBuffer` in the spec — this is `save-current-buffer` which is similar to `save-excursion` but without restoring point. Add a simple implementation: push a `SaveExcursion` without a marker, or handle it as an unwind-protect.

Actually, check what GNU does for `Bsave_current_buffer`. It calls `record_unwind_current_buffer()` which pushes `SPECPDL_UNWIND` with `set_buffer_if_live` as the restore function. The simplest approach: push a `SpecBinding::UnwindProtect` that restores the current buffer, or add a small `SaveCurrentBuffer` variant.

For now, use an approach that matches the existing behavior — push onto specpdl with a variant that `unbind_to` can handle. Since the spec didn't include `SaveCurrentBuffer`, add it as a sub-case of `SaveExcursion` with marker_id = 0 (sentinel meaning "don't restore point") or as a new variant. The cleanest is a new variant:

```rust
// Add to SpecBinding in Task 1 if not already present:
SaveCurrentBuffer { buffer_id: BufferId },
```

And in unbind_to:
```rust
SpecBinding::SaveCurrentBuffer { buffer_id } => {
    self.restore_current_buffer_if_live(buffer_id);
}
```

- [ ] **Step 7: Rewrite UnwindProtectPop opcode (line 1806-1808)**

Before:
```rust
Op::UnwindProtectPop => {
    let cleanup = stk!().pop().unwrap_or(Value::NIL);
    specpdl.push(VmUnwindEntry::Cleanup { cleanup });
}
```

After:
```rust
Op::UnwindProtectPop => {
    let cleanup = stk!().pop().unwrap_or(Value::NIL);
    // For bytecoded unwind-protect, the cleanup is a compiled function,
    // not raw forms. Push it as a callable — unbind_to will call
    // sf_progn_value on it, but since it's a bytecode function value
    // (not a cons form), sf_progn_value won't work.
    //
    // We need a different approach for VM unwind-protect: the cleanup
    // is a callable Value (lambda/bytecode), not a form list.
    // Option: add an UnwindProtectCallable variant that calls apply()
    // instead of sf_progn_value().
    unwind_depth_stack.push(self.ctx.specpdl.len());
    self.ctx.specpdl.push(SpecBinding::UnwindProtect {
        forms: cleanup,
        lexenv: self.ctx.lexenv,
    });
}
```

**IMPORTANT:** The VM's cleanup is a **callable** (bytecode function), not raw forms. The interpreter's cleanup is raw forms evaluated with `sf_progn_value`. The `unbind_to` handler for `UnwindProtect` currently calls `sf_progn_value(forms)`.

For a callable (bytecoded) cleanup, `sf_progn_value` on a lambda/bytecode Value won't work — it expects a cons list of forms.

Two approaches:
1. **Two variants**: `UnwindProtectForms { forms, lexenv }` for interpreter, `UnwindProtectCallable { cleanup }` for VM. `unbind_to` calls `sf_progn_value` for forms, `apply` for callable.
2. **One variant with detection**: `UnwindProtect { cleanup: Value, lexenv: Value }`. In `unbind_to`, check if `cleanup` is a cons (forms to progn) or a function (call with apply).

Approach 2 is simpler. In `unbind_to`:

```rust
SpecBinding::UnwindProtect { forms: cleanup, lexenv } => {
    let saved_lexenv = self.lexenv;
    self.lexenv = lexenv;
    if cleanup.is_cons() || cleanup.is_nil() {
        // Interpreter path: cleanup is a list of forms
        let _ = self.sf_progn_value(cleanup);
    } else {
        // VM path: cleanup is a callable (bytecode function)
        let _ = self.apply(cleanup, vec![]);
    }
    self.lexenv = saved_lexenv;
}
```

This matches GNU's pattern where the unwind function is called directly.

- [ ] **Step 8: Rewrite resume_nonlocal**

Find `resume_nonlocal` in vm.rs. Currently it walks the VM's `specpdl: &[VmUnwindEntry]` to unwind. After: it calls `ctx.unbind_to(spec_depth)` where `spec_depth` comes from the `ResumeTarget`.

The `ResumeTarget` variants (`VmCatch`, `VmConditionCase`) already store `spec_depth: usize`. After the change, this refers to `Context.specpdl` depth (which it already does, since the VmUnwindEntry::DynamicBinding used to store ctx.specpdl.len()). Verify that `spec_depth` in handler setup captures `ctx.specpdl.len()`.

In `resume_nonlocal`, replace the VmUnwindEntry walk with:
```rust
self.ctx.unbind_to(resume_target.spec_depth);
```

Also clear `unwind_depth_stack` appropriately, and truncate `bc_buf` and reset pc per the existing logic.

- [ ] **Step 9: Remove VmUnwindEntry and all related methods**

Delete:
- `enum VmUnwindEntry` (line 30-50)
- `fn collect_specpdl_roots` (line 176)
- `fn unwind_specpdl_all` (line 3764)
- `fn unwind_specpdl_n` (line 3768)
- `fn unwind_specpdl_to` (line 3777)
- `fn restore_unwind_entry` (line 3789)
- `fn restore_saved_restriction` (line 3829)
- All `specpdl: &mut Vec<VmUnwindEntry>` parameters from `run_loop`, `run_frame`, and any helper functions

- [ ] **Step 10: Update all call sites that pass the VM specpdl parameter**

Search for all functions that took `specpdl: &[VmUnwindEntry]` or `specpdl: &mut Vec<VmUnwindEntry>` and update their signatures and callers. This includes `with_frame_call_roots`, `with_frame_roots`, and related helpers.

- [ ] **Step 11: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

This is the most complex step — expect multiple iterations to get everything compiling.

- [ ] **Step 12: Commit**

```bash
git add neovm-core/src/emacs_core/bytecode/vm.rs neovm-core/src/emacs_core/eval.rs
git commit -m "Rewrite VM to use unified specpdl instead of VmUnwindEntry"
```

---

### Task 6: Move backtrace onto specpdl

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs` (push_active_call_frame, pop_active_call_frame, push_runtime_backtrace_frame, pop_runtime_backtrace_frame, trace_roots, all callers)
- Modify: `neovm-core/src/emacs_core/misc.rs` (backtrace builtins)
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs` (VM call_function backtrace push)

- [ ] **Step 1: Create specpdl backtrace push/pop helpers**

In eval.rs, add:

```rust
    pub(crate) fn push_backtrace_frame(&mut self, function: Value, args: &[Value]) {
        self.specpdl.push(SpecBinding::Backtrace {
            function,
            args: LispArgVec::from(args),
            debug_on_exit: false,
        });
    }

    pub(crate) fn pop_backtrace_frame(&mut self) {
        // Pop backwards until we find our Backtrace entry.
        // In normal flow, it should be at or near the top.
        // But there may be GcRoot entries above it.
        // The safest approach: record specpdl depth before push,
        // unbind_to that depth on pop.
        // However, for backtrace specifically, we can just scan.
        if let Some(pos) = self.specpdl.iter().rposition(|b| matches!(b, SpecBinding::Backtrace { .. })) {
            // If it's at the top, just pop
            if pos == self.specpdl.len() - 1 {
                self.specpdl.pop();
            }
            // Otherwise, there are entries above it (e.g., GcRoot) —
            // remove the backtrace entry and keep the rest.
            // This shouldn't happen in normal flow.
        }
    }
```

Actually, the cleaner approach matching GNU: the caller saves `specpdl.len()` before pushing the backtrace entry, and calls `unbind_to(saved)` after. This naturally pops the backtrace and any GC roots pushed during the call:

```rust
    pub(crate) fn push_backtrace_frame(&mut self, function: Value, args: &[Value]) {
        self.specpdl.push(SpecBinding::Backtrace {
            function,
            args: LispArgVec::from(args),
            debug_on_exit: false,
        });
    }
```

No separate pop method — callers use `unbind_to(count)`.

- [ ] **Step 2: Replace all push_active_call_frame + push_runtime_backtrace_frame pairs**

Search for all sites that call `push_active_call_frame` and `push_runtime_backtrace_frame_from_active_call`. Replace each pair with:

```rust
let bt_count = self.specpdl.len();
self.push_backtrace_frame(function, &args);
// ... call body ...
self.unbind_to(bt_count);
```

There are ~15 call sites in eval.rs (lines 6518, 6529, 6597, 8955-8957, 8967-8983, 9030-9038, 9061-9065, 9437-9446, etc.) and in vm.rs (`call_function`).

Each site currently does:
```rust
self.push_active_call_frame(func, callable, &args);
self.push_runtime_backtrace_frame_from_active_call(func);
// ... body ...
self.pop_runtime_backtrace_frame();
self.pop_active_call_frame();
```

Replace with:
```rust
let bt_count = self.specpdl.len();
self.push_backtrace_frame(func, &args);
// ... body ...
self.unbind_to(bt_count);
```

The `with_runtime_backtrace_frame_from_active_call` helper method can be replaced with a simpler pattern too.

- [ ] **Step 3: Rewrite backtrace builtins in misc.rs**

Find `runtime_backtrace_frames_from_base` (misc.rs line 639) and related functions. Rewrite to walk specpdl instead of `runtime_backtrace`:

```rust
fn specpdl_backtrace_frames(eval: &Context) -> Vec<&SpecBinding> {
    eval.specpdl.iter().rev()
        .filter(|b| matches!(b, SpecBinding::Backtrace { .. }))
        .collect()
}
```

Update `builtin_backtrace_frame`, `builtin_backtrace_frame_internal`, `builtin_backtrace_frames_from_thread`, and `mapbacktrace` to use this.

For each backtrace frame access, the old code did:
```rust
let args = eval.runtime_backtrace_frame_args(frame);
```

Now the args are directly in the `SpecBinding::Backtrace { args, .. }` variant.

- [ ] **Step 4: Update trace_roots — remove old structure tracing**

In trace_roots (line 4353-4367), delete the `active_call_roots` and `runtime_backtrace` tracing loops. These are now covered by the `SpecBinding::Backtrace` tracing added in Task 1.

- [ ] **Step 5: Delete old structures**

Delete from eval.rs:
- `struct RuntimeBacktraceFrame` (line 181-186)
- `struct ActiveCallFrame` (line 189-204)
- `runtime_backtrace: Vec<RuntimeBacktraceFrame>` field (line 1369)
- `active_call_roots: Vec<ActiveCallFrame>` field (line 1366)
- `fn push_runtime_backtrace_frame_from_active_call` (line 8829)
- `fn pop_runtime_backtrace_frame` (line 8843)
- `fn runtime_backtrace_frame_args` (line 8847)
- `fn push_active_call_frame` (line 8857)
- `fn pop_active_call_frame` (line 8867)
- `fn with_runtime_backtrace_frame_from_active_call` (line 8950)
- `fn push_active_call_arg` (around line 8877)
- Initialization of these fields in Context::new() (lines 4093-4094, 4227-4228)

Also delete `ActiveCallFrame`-related functions that were used in lambda call setup (`begin_lambda_call_in_state`, etc.) — replace `extra_roots` usage with `SpecBinding::GcRoot` pushes.

- [ ] **Step 6: Build and verify**

```bash
cargo build --workspace 2>&1 | tail -15
```

This will likely require multiple iterations — there are many call sites.

- [ ] **Step 7: Run tests**

```bash
cargo nextest run --workspace 2>&1 > /tmp/test-results.txt; tail -30 /tmp/test-results.txt
```

- [ ] **Step 8: Commit**

```bash
git add neovm-core/
git commit -m "Move backtrace and call frames onto specpdl"
```

---

### Task 7: Clean up and verify

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs`
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs`

- [ ] **Step 1: Delete unbind_to_in_state if no longer needed**

Check if `unbind_to_in_state` (eval.rs line 10494) is still called anywhere. If the VM now uses `ctx.unbind_to()` directly, this standalone version may be dead code. If so, delete it.

```bash
grep -rn "unbind_to_in_state" neovm-core/src/ | grep -v "^.*:.*fn unbind_to_in_state"
```

- [ ] **Step 2: Search for any remaining references to deleted structures**

```bash
grep -rn "VmUnwindEntry\|RuntimeBacktraceFrame\|ActiveCallFrame\|active_call_roots\|runtime_backtrace\|push_active_call_frame\|pop_active_call_frame\|push_runtime_backtrace" neovm-core/src/
```

Fix any remaining references.

- [ ] **Step 3: Search for any remaining with_gc_scope references (from previous migration)**

```bash
grep -rn "with_gc_scope\|push_eval_root\|save_eval_roots\|restore_eval_roots" neovm-core/src/
```

Should return nothing.

- [ ] **Step 4: Full build and test**

```bash
cargo build --workspace 2>&1 | tail -15
cargo nextest run --workspace 2>&1 > /tmp/test-results.txt; tail -30 /tmp/test-results.txt
```

- [ ] **Step 5: Fresh build test**

```bash
cargo xtask fresh-build 2>&1 | tail -30
```

- [ ] **Step 6: Commit any final cleanups**

```bash
git add neovm-core/
git commit -m "Clean up after specpdl unification"
```

---

## Execution Order

Tasks must be executed in order:
1. **Task 1** — add new SpecBinding variants (foundation for everything else)
2. **Tasks 2-4** — interpreter special forms (independent of each other, but depend on Task 1)
3. **Task 5** — VM rewrite (depends on Task 1, can overlap with Tasks 2-4)
4. **Task 6** — backtrace migration (depends on Tasks 1 and 5)
5. **Task 7** — cleanup (depends on all above)

Tasks 2, 3, and 4 are independent of each other and can be done in parallel.
Task 5 is the largest and most complex — expect it to take the most time.
Task 6 has the widest blast radius (many call sites to update).
