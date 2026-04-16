# Lexenv Leak Investigation

**Date:** 2026-04-16
**Status:** Root cause identified, fix not yet implemented

## Bug

12,372 lexenv leaks during Doom startup. Every `let` form with lexical bindings leaks — stale binding entries accumulate in `self.lexenv`, causing closures to capture oversized environments.

**Symptom:** `_rest` warning in `*Compile-Log*` from `cconv-make-interpreted-closure` seeing stale `tail` bindings in closure environments.

## Root Cause (confirmed via rust-gdb)

In `sf_let_value_named` (eval.rs:7256), the `LexicalEnv` save happens at line 7370:
```rust
let saved = self.lexenv;  // line 7370
self.specpdl.push(SpecBinding::LexicalEnv { old_lexenv: saved });
```

But `self.lexenv` at this point is ALREADY different from what it was at function entry (line 7256). The init value loop (lines 7276-7350) runs `eval_sub(init_form)` which can recursively call `sf_let_value_named`, and each recursive call has the same cascading leak.

**Confirmed via gdb hardware watchpoint:** the `MISMATCH` between entry-time lexenv and save-time lexenv is caused by nested `let` evaluations during init form computation.

## GNU Emacs Comparison

GNU `Flet` (eval.c:1129-1194):
1. Lines 1153-1164: Compute ALL init values (can change `Vinternal_interpreter_environment`)
2. Line 1167: `lexenv = Vinternal_interpreter_environment` (LOCAL variable, captures post-init state)
3. Lines 1170-1186: Build new lexenv by consing bindings onto LOCAL `lexenv` — does NOT modify `Vinternal_interpreter_environment`
4. Line 1188-1190: ONE atomic `specbind(Qinternal_interpreter_environment, lexenv)` — saves current global and replaces with new

**Key difference:** GNU builds the new lexenv in a LOCAL variable and does ONE atomic swap. Neomacs modifies `self.lexenv` directly via `bind_lexical_value_rooted_in_specpdl` during the binding phase, making each binding visible immediately.

## Fix Direction

Restructure `sf_let_value_named` to match GNU's pattern:
1. Compute init values into a temporary array
2. Build new lexenv as a local value by consing bindings onto current `self.lexenv`
3. Push `SpecBinding::LexicalEnv { old_lexenv: self.lexenv }` (saving current)
4. Set `self.lexenv = new_lexenv` in one assignment
5. Execute body
6. `unbind_to` restores `self.lexenv`

This matches GNU's atomic save-and-replace via `specbind`.

## Files

- `neovm-core/src/emacs_core/eval.rs:7256-7385` (sf_let_value_named)
- Related: `bind_lexical_value_rooted_in_specpdl` (line 1709) — currently modifies lexenv directly
