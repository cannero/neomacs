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

### GNU startup ownership

GNU startup builds one callable world in C during normal initialization.

- `emacs.c` calls `syms_of_*` initialization functions during startup.
- modules like `window.c` register public entry points with `defsubr`.
- `defsubr` in `lread.c` interns the symbol and writes the subr directly into
  the symbol's function cell with `set_symbol_function`.

That means the public callable surface is not a secondary cache layered on top
of the runtime. It is the runtime.

### GNU bytecode call path

GNU bytecode does not resolve ordinary public primitive calls through a
separate VM-only registry.

In `bytecode.c`, the generic `Bcall*` path:

1. looks at the function object on the stack
2. follows the symbol's function cell
3. uses a fast path for closures and subrs when available
4. otherwise falls through to `funcall_general`

In `eval.c`, `funcall_general` again starts from the function object and its
symbol function cell. If the target is a subr, it calls `funcall_subr`; if it
is nil, it signals `void-function`.

So GNU's rule is:

- direct bytecode instructions are optimizations
- ordinary primitive callability still depends on the same public function
  surface the interpreter uses

This is exactly why a reduced harness is architecturally wrong for Neomacs
compatibility testing.

### GNU pdump behavior

GNU pdump does not introduce a second callable namespace for bytecode.

What pdump hooks repair after dump load are C-owned runtime structures, for
example:

- `init_eval_once` registers `init_eval_once_for_pdumper`
- that hook reinitializes evaluator-owned specpdl storage after pdump load

This is a key distinction:

- pdump repairs C runtime state after load
- pdump does not replace the Lisp-visible callable surface with a smaller one

So the Neomacs pattern of hand-replacing the obarray for VM tests is not GNU
pdump-shaped either.

### GNU test strategy

GNU's own tests also reflect this design.

- `eval-tests.el` exercises evaluator and bytecode behavior inside ordinary
  Emacs.
- `bytecomp-tests.el` compares interpreted and byte-compiled results by
  compiling a lambda and `funcall`ing it in the normal runtime.
- `comp-tests.el` explicitly checks that primitives with no dedicated bytecode
  are still callable.

That last point matters. GNU's test suite assumes compiled code can call normal
primitives through the shared runtime surface; it does not rely on a special VM
test harness with a reduced public function namespace.

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

## Deep Design Recommendation

The current design is wrong in two different directions at once:

- it is not GNU-shaped enough for compatibility testing
- it is not small or explicit enough to be a clean unit-only harness

So the right fix is not to keep teaching the VM more fallback tricks. The fix
is to split harness responsibilities clearly.

### What GNU Emacs effectively does

GNU has one real runtime surface:

- symbols live in one obarray
- `defsubr` writes real function cells into that surface
- interpreter and bytecode both call into that same callable world
- pdump repairs C-owned runtime state, not the Lisp-visible callable namespace

Tests may initialize less editor state, but GNU does not create a separate
"bytecode-only function namespace" where ordinary public subrs disappear.

### What Neomacs should do

Neomacs should have two explicit evaluator constructors, not one ambiguous one.

#### 1. Full runtime VM test context

This should be the default for `vm_eval_str`, `vm_eval_with_init_str`, and any
test that claims "shared runtime state" or GNU compatibility.

Shape:

- start from `Context::new()`
- keep the builtin subr registry
- keep builtin function cells in the obarray
- reset mutable editor/runtime state only where test isolation requires it

That gives VM tests the same public callable surface as ordinary evaluator
calls, which is the GNU shape.

#### 2. Minimal opcode/unit harness

This should only be used for tests that are intentionally about:

- direct bytecode opcodes
- stack/unwind mechanics
- hand-built bytecode functions
- GC/rooting invariants

That harness should be renamed so its semantics are obvious, for example:

- `new_vm_opcode_harness()`
- `new_minimal_vm_harness()`

If it keeps a reduced function surface, that should be by design and by name,
not hidden behind the default VM helper.

### Why renaming matters

`new_vm_harness()` currently sounds like "the right runtime for VM tests".
That is false. Right now it is a partial synthetic evaluator state with no GNU
equivalent.

Renaming the minimal version is part of the fix because it forces the codebase
to distinguish:

- VM compatibility tests
- VM unit mechanics tests

Those are not the same thing.

### Recommended migration plan

1. Change `new_vm_harness()` to build from `Context::new()` and only reset
   mutable runtime/editor state.
2. Move the current stripped constructor body to a new explicitly named helper
   such as `new_minimal_vm_harness()`.
3. Keep `vm_eval_str`, `vm_eval_lexical_str`, and `vm_eval_with_init_str`
   on the full runtime harness.
4. Convert only the truly low-level tests to the minimal harness.
5. Add one guard test that proves the full VM harness can call ordinary public
   subrs like:
   - `selected-window`
   - `fset`
   - `defvaralias`
   - `func-arity`

### Whether pdump snapshotting should be involved

For test isolation, pdump snapshot/restore is architecturally closer to GNU
than hand-editing the evaluator surface, because it preserves a coherent
runtime. But it is probably not the first refactor step here.

The first step should be simpler:

- make the default VM harness a full `Context::new()` runtime
- only then decide whether repeated VM tests should clone a cached snapshot for
  speed

Snapshotting is an optimization and isolation tool. It is not a substitute for
having the correct callable surface.

### What should stay optimized

Neomacs should still keep direct VM opcodes where they genuinely match GNU's
bytecode fast paths. The mistake is not "having fast paths"; the mistake is
letting those fast paths hide that the generic callable surface is wrong.

So the intended ownership should be:

- direct opcodes are performance optimizations
- public primitive callability is owned by the shared evaluator surface
- VM compatibility tests must validate both paths

### Bottom line

GNU's rule is simple: bytecode runs inside the same Lisp world as the
interpreter.

Neomacs should adopt the same rule:

- full VM compatibility tests must run against the same builtin function
  surface as `Context::new()`
- any reduced harness must be opt-in, narrowly named, and never be the default
