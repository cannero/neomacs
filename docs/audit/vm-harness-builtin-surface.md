# VM Harness Builtin Surface Audit

**Date**: 2026-03-30

## Problem

Current `main` has a broad VM bootstrap mismatch that shows up as:

- `ERR (void-function (selected-window))`
- `ERR (void-function (fset))`
- `ERR (void-function (defvaralias))`

This is not a window-only bug. It is a harness/runtime split: the VM test
harness does not build the same builtin function surface that normal Neomacs
startup builds, so many bytecode calls run in an evaluator with no real subr
registry and almost no public function cells.

## GNU Emacs Design

GNU Emacs does not maintain a separate "thin VM harness" surface for bytecode.

- `defsubr` in `src/lread.c` interns the symbol and installs the subr in the
  symbol's function cell with `set_symbol_function`.
- `syms_of_window` in `src/window.c` registers `selected-window` with
  `defsubr (&Sselected_window)`.
- Interpreter and bytecode ultimately share the same callable runtime surface.
  Bytecode does not run against a reduced public function-cell table.

That matters here because `(selected-window)` is just an ordinary public subr
call in GNU Emacs. There is no special VM-only fallback required for it to be
callable.

## Neomacs Design Today

### Normal startup path

`Context::new()` in `neovm-core/src/emacs_core/eval.rs` does:

1. `Self::new_inner(true)`
2. `builtins::init_builtins(&mut ctx)`

That gives normal runtime both of the things GNU relies on:

- a populated subr registry
- builtin function cells in the obarray

### VM harness path

`Context::new_vm_harness()` in `neovm-core/src/emacs_core/eval.rs` does not
match that shape:

1. `Self::new_inner(true)`
2. replaces `ev.obarray` with `Obarray::new()`
3. resets runtime/editor state
4. calls only `ev.materialize_public_evaluator_function_cells()`

`new_inner()` initializes `subr_registry` as an empty `Vec`, so the VM harness
starts with no builtin subrs registered. Then it replaces the obarray and only
re-materializes the public evaluator-owned forms. That helper exposes:

- public special forms like `if`, `let`, `condition-case`
- evaluator callable `throw`

It does not expose ordinary public builtins like:

- `selected-window`
- `fset`
- `defvaralias`
- `func-arity`

So the harness is missing both GNU compatibility layers:

- no registered builtin subrs
- no normal builtin function-cell surface

## Why Some VM Tests Still Pass

The VM currently has three different call shapes:

1. direct opcodes with inline Rust implementations, like `Op::Add`
2. VM proxy opcodes that bounce into builtin dispatch, like `Op::Fset`
3. generic symbol calls compiled as `Op::Call`

Only the first category survives the thin harness reliably.

### Direct opcodes

Examples like `(+ 1 2)` compile to direct VM arithmetic and still pass because
the VM implements them internally and only consults function lookup to honor
shadowing.

### Proxy opcodes

Examples like `fset` or `symbol-value` compile to VM opcodes that eventually
call `dispatch_vm_builtin`, which delegates back to shared builtin dispatch.
That path still requires a populated builtin subr registry, so it fails in the
harness.

### Generic calls

Examples like `(selected-window)` compile to a normal `Op::Call` on the symbol
`selected-window`. That goes through `funcall_general(Value::Symbol(...))`,
which expects either:

- a real function cell in the obarray, or
- a registered builtin subr fallback

The VM harness has neither, so it resolves to `void-function`.

## Reproduced Evidence

Focused `cargo nextest` runs on current `main` reproduce all of these:

- `vm_frame_selected_window_builtins_use_shared_runtime_state`
  fails with `ERR (void-function (selected-window))`
- `vm_function_mutator_builtins_use_shared_function_state`
  fails with `ERR (void-function (fset))`
- `vm_variable_lookup_builtins_use_shared_dynamic_and_buffer_local_state`
  fails with `ERR (void-function (defvaralias))`
- `vm_addition`
  still passes

That last passing test is important because it shows why this problem can stay
hidden: opcode-local implementations make the VM look healthier than its real
GNU-compatible callable surface actually is.

## Audit Conclusion

This is an architectural mismatch, not a single missing builtin.

`Context::new_vm_harness()` is not GNU-shaped. It creates a private evaluator
surface that is materially different from normal startup, then the VM partly
papers over that split with direct opcode implementations.

That means:

- current VM-harness results are not a trustworthy oracle for GNU-compatible
  builtin callability
- failures like `selected-window` are only the visible edge of a wider
  bootstrap problem
- the remaining split is between "VM bytecode runtime" and "normal builtin
  callable surface", not between keyboard code and window code

## Required Refactor Direction

The fix direction should follow GNU Emacs's ownership model:

1. `new_vm_harness()` should build the same builtin runtime surface as
   `Context::new()`, not a reduced evaluator-only one.
2. If the harness still needs isolation for tests, it should reset editor state
   after full builtin initialization, not replace the obarray/subr surface.
3. `materialize_public_evaluator_function_cells()` should remain a narrow
   helper for evaluator-owned forms, not a surrogate for builtin bootstrap.
4. VM compatibility tests should add paired coverage for:
   - direct opcode call survives
   - proxy builtin call survives
   - generic public subr call survives

Until that is done, Neomacs's VM runtime still differs from GNU Emacs in a
fundamental way: bytecode is not running against the same public callable
surface as the ordinary evaluator.
