# Plan: VM Harness Runtime Unification

**Date**: 2026-03-30
**Status**: In progress

## Recommendation

Neomacs should stop using a reduced evaluator surface as the default VM test
harness.

The GNU-compatible target is:

- one callable Lisp-visible runtime surface
- one bytecode engine that runs inside that surface
- optional reduced harnesses only for explicitly low-level VM unit tests

So the refactor should make the default VM harness a full runtime and move the
current stripped constructor behind an explicit minimal-harness name.

## Current progress

The first execution slice is in:

- `Context::new_vm_runtime_harness()` now exists as the full callable-surface
  test constructor.
- `Context::new_minimal_vm_harness()` now holds the old stripped constructor
  body.
- `Context::new_vm_harness()` now routes to the runtime harness, so the default
  VM test path uses the real builtin surface.
- source-form VM helpers now use the runtime harness.
- the low-level evaluator-drop test now calls the minimal harness explicitly.

The second execution slice is also in:

- `execute_manual_vm` and `execute_manual_vm_built` now use the minimal
  harness explicitly.
- all remaining direct `Context::new_vm_harness()` test call sites were
  classified and moved to either `new_vm_runtime_harness()` or
  `new_minimal_vm_harness()`.
- `vm_test.rs` now has an explicit runtime-harness invariant test covering
  `selected-window`, `fset`, `defvaralias`, and `func-arity`.
- the full-runtime harness also exposed one stale VM test assumption around
  GNU recursive `load` depth; that test was corrected to match `lread.c`.

Focused verification after this slice:

- `vm_frame_selected_window_builtins_use_shared_runtime_state`
- `vm_function_mutator_builtins_use_shared_function_state`
- `vm_variable_lookup_builtins_use_shared_dynamic_and_buffer_local_state`
- `vm_switch_branches_using_hash_table_jump_table`
- `vm_throw_restores_saved_stack_before_resuming_catch`
- `evaluator_drop_clears_owned_thread_locals`

All six passed.

Focused verification after the second slice:

- `vm_runtime_harness_exposes_public_builtin_surface`
- `vm_switch_branches_using_hash_table_jump_table`
- `vm_throw_restores_saved_stack_before_resuming_catch`
- `vm_bytecode_wrong_arity_matches_gnu_entry_check`
- `vm_gnu_arg_descriptor_preserves_optional_and_rest_slots`
- `vm_compiled_autoload_registration_updates_shared_autoload_manager`
- `vm_compiled_load_uses_shared_runtime_and_restores_load_file_name`
- `vm_compiled_load_allows_gnu_normal_recursive_load_depth`
- `vm_compiled_load_signals_after_gnu_recursive_load_limit`

All targeted checks passed.

## Why this refactor is necessary

The current `Context::new_vm_harness()` mixes two goals that should not be
mixed:

- isolate mutable runtime/editor state for tests
- reduce the evaluator surface

GNU Emacs does the first, but not the second.

GNU architecture:

- `defsubr` writes subrs into real symbol function cells
- startup builds one public callable surface through `syms_of_*`
- `exec_byte_code` uses fast paths where available, then falls back to the same
  function world through `funcall_general`
- pdump repairs C-owned runtime state after load, not a second Lisp-visible
  namespace

Current Neomacs harness behavior diverges from that:

- it replaces the obarray
- it leaves `subr_registry` empty
- it restores only evaluator-owned forms like `if`, `let`, and `throw`

That means ordinary public primitives disappear under the default VM harness,
which is architecturally wrong even when some opcode-heavy tests still pass.

## Non-goals

This plan is not trying to:

- remove direct VM opcodes
- make every VM test use a bootstrapped or loadup-complete evaluator
- replace unit-style manual bytecode tests with higher-level evaluator tests
- delete minimal harnesses entirely

Direct opcodes remain valid. The requirement is only that generic callable
semantics use the same public function surface as the ordinary evaluator.

## Target architecture

Neomacs should expose two clearly different test-only constructors.

### 1. Full runtime VM harness

Purpose:

- GNU compatibility tests
- source-form VM execution tests
- tests that claim "shared runtime state"
- tests that exercise ordinary public builtins, keymaps, windows, frames,
  variable/function mutation, interactive behavior, or command state

Properties:

- starts from `Context::new()`
- preserves builtin function cells in the obarray
- preserves builtin `subr_registry`
- preserves the same callable world as ordinary evaluator execution
- resets only mutable runtime/editor state if isolation actually requires it

Suggested name during migration:

- `new_vm_runtime_harness()`

### 2. Minimal VM harness

Purpose:

- direct opcode tests
- manual `ByteCodeFunction` tests
- stack/unwind balancing tests
- GC rooting invariants
- low-level VM mechanics that do not claim GNU public-callable fidelity

Properties:

- may keep a reduced evaluator surface
- must be explicitly named and opt-in
- must never be the default helper for compatibility-oriented VM tests

Suggested name:

- `new_minimal_vm_harness()`

## Naming and API migration

The current `new_vm_harness()` name is misleading because it sounds like the
default correct runtime for VM tests. It is not.

Recommended migration path:

1. Add `new_minimal_vm_harness()` as the new name for the current stripped
   constructor body.
2. Add `new_vm_runtime_harness()` that starts from `Context::new()`.
3. Migrate helper functions in `bytecode/vm_test.rs` to use the runtime
   harness.
4. After migration, either:
   - rename `new_vm_runtime_harness()` to `new_vm_harness()`, or
   - delete `new_vm_harness()` entirely and keep only explicit names

The second option is cleaner because it avoids another ambiguous default.

## Constructor design

### Runtime harness implementation

Phase 1 should prefer correctness over cleverness:

- start from `Context::new()`
- do not replace the obarray
- do not clear the builtin subr registry
- do not re-synthesize evaluator function cells manually

Then only reset state that is genuinely test-isolation state rather than
callable-runtime state. Examples that may reasonably be reset:

- `input_rx`
- `wakeup_fd`
- host/display handles
- transient process/timer/input channels
- runtime counters like `gc_pending`
- mutable command-loop pending input state

Examples that should not be reset in the runtime harness:

- builtin function cells
- builtin `subr_registry`
- ordinary default variable surface created by `Context::new()`
- default frames/buffers unless a specific test needs custom setup

New contexts already provide cross-test isolation. So the default bias should
be: reset less, not more.

### Minimal harness implementation

The current constructor body is still useful for low-level VM testing. It
should survive, but only behind the minimal-harness name.

Important rule:

- any helper that uses the minimal harness must do so intentionally

No compatibility helper should call it by accident.

## Test helper migration

The most important change is not the constructor itself. It is the helper layer
that controls which harness most tests actually use.

Current helpers in `bytecode/vm_test.rs` are roughly split between:

- source-form helpers like `vm_eval_str`, `vm_eval_lexical_str`,
  `vm_eval_with_init_str`
- low-level manual-bytecode helpers like `execute_manual_vm`
  and `execute_manual_vm_built`

Recommended mapping:

### Runtime helpers

These should use the full runtime harness:

- `with_vm_eval_state`
- `vm_eval_str`
- `vm_eval_lexical_str`
- `vm_eval_with_init_str`

These helpers are the compatibility surface. If they use a reduced harness,
they stop being meaningful as GNU bytecode tests.

### Minimal helpers

These should use the minimal harness:

- `execute_manual_vm`
- `execute_manual_vm_built`
- direct hand-crafted opcode tests that never claim public builtin fidelity

If a manual-bytecode test really needs the full runtime, it should construct
that explicitly rather than inheriting the minimal harness accidentally.

## Test migration heuristics

Use this rule of thumb when moving existing tests.

Move to runtime harness if the test:

- compiles source forms
- calls ordinary public builtins
- uses windows, frames, buffers, keymaps, hooks, commands, or variable/function
  mutation
- claims "shared runtime state"
- is intended to compare VM behavior to ordinary evaluator behavior

Keep on minimal harness if the test:

- builds `ByteCodeFunction` by hand
- asserts exact stack/unwind mechanics
- tests opcode lowering or execution directly
- would still make sense if all symbol/function lookup were stubbed out

## Performance strategy

The first implementation should just use `Context::new()` for correctness.

After the semantics are fixed, optimize harness creation if necessary.

The best optimization path is already present in the codebase:

- `pdump::snapshot_evaluator`
- `pdump::restore_snapshot`
- `pdump::clone_evaluator`

That suggests the right long-term speed path:

1. build one canonical runtime template from `Context::new()`
2. snapshot it once
3. restore that snapshot for runtime harness tests

This is much closer to GNU's model than hand-editing the callable surface. It
preserves a coherent runtime and only optimizes construction cost.

But this should be a second step, not the first. Correctness first.

## Interaction with existing plans

This refactor is downstream of the existing function-dispatch unification work,
not a replacement for it.

Relationship to `docs/plans/funcall-general-unification.md`:

- `funcall_general` unification makes interpreter and VM share call dispatch
- VM harness runtime unification makes tests run against the same callable
  surface that dispatch expects

Without this harness refactor, the call-unification design remains partly
hidden by the default VM test environment.

Relationship to `docs/plans/error-condition-unification.md`:

- the condition-runtime work unified semantic dispatch across interpreter and VM
- this plan does the same kind of unification for the callable test surface

Both are the same architectural lesson: one semantic owner, not parallel local
surfaces.

## Phased implementation plan

### Phase 0: Codify invariants

Add explicit documentation comments around:

- `Context::new()`
- `Context::new_vm_harness()` or its replacements
- `materialize_public_evaluator_function_cells()`

State clearly:

- `init_builtins()` owns the builtin callable surface
- evaluator-only function-cell materialization is not a builtin bootstrap path

### Phase 1: Split constructors without changing test helpers

Implement:

- `new_minimal_vm_harness()` with the current stripped body
- `new_vm_runtime_harness()` with `Context::new()` semantics

Keep existing helpers unchanged for one short step so the change is easy to
review and bisect.

### Phase 2: Move compatibility helpers to runtime harness

Switch these helpers first:

- `with_vm_eval_state`
- `vm_eval_str`
- `vm_eval_lexical_str`
- `vm_eval_with_init_str`

At this point, source-form VM tests should run against the full runtime.

Expected immediate effect:

- currently failing builtin-call tests like `selected-window`, `fset`,
  `defvaralias`, and `func-arity` should start behaving like ordinary runtime
  calls
- some previously hidden differences may surface in tests that were only
  passing because the reduced harness masked them

That is desirable. It means the harness is no longer lying.

### Phase 3: Migrate low-level tests to minimal harness

Update explicit low-level helpers:

- `execute_manual_vm`
- `execute_manual_vm_built`

Then move any direct `Context::new_vm_harness()` call sites to either:

- runtime harness
- minimal harness

based on the heuristics above.

### Phase 4: Add guard regressions

Add a small set of invariant tests proving the runtime harness really exposes
ordinary public primitives, for example:

- `(selected-window)`
- `(fset 'x 'car)`
- `(defvaralias 'a 'b)`
- `(func-arity 'car)` or another stable builtin

These should live near the VM helper tests so future harness regressions are
obvious.

Also add one negative/explicit minimal-harness test if the reduced harness is
expected to stay reduced, so the distinction is documented in code rather than
implicit.

### Phase 5: Optional snapshot optimization

If runtime harness creation is measurably expensive:

- cache a `DumpContextState` snapshot of a canonical `Context::new()`
- restore from that snapshot in runtime harness helpers

Do not do this before the semantic split is clear and tested.

## Acceptance criteria

This refactor is complete when:

- source-form VM helpers no longer use the reduced harness
- ordinary public builtins are callable under the default VM compatibility
  helpers
- low-level manual bytecode tests still have an explicit minimal harness
- `materialize_public_evaluator_function_cells()` is no longer treated as a
  substitute for builtin bootstrap
- the remaining distinction between runtime and minimal harnesses is explicit in
  code, docs, and tests

## Risks

### Risk 1: Hidden dependency on stripped startup state

Some current VM tests may accidentally rely on the reduced harness having fewer
bindings or less pre-seeded state.

That is not a reason to keep the wrong default. It only means those tests need
to declare whether they are:

- compatibility tests
- low-level VM mechanics tests

### Risk 2: Slower test startup

Using `Context::new()` may increase per-test setup cost.

Mitigation:

- accept the cost first
- optimize later with snapshot restore if needed

### Risk 3: More real failures become visible

Once the runtime harness is correct, more GNU-compatibility bugs may surface.

That is expected and desirable. The purpose of this refactor is to stop hiding
them behind a synthetic evaluator surface.

## Bottom line

GNU Emacs has one callable runtime surface, and bytecode runs inside it.

Neomacs should do the same in its default VM compatibility tests:

- full runtime harness by default
- minimal harness only by explicit opt-in
- no more reduced callable surface hidden behind `new_vm_harness()`
