# Error / Condition System Audit

Date: 2026-03-29

Scope:
- `signal`, `condition-case`, `handler-bind`, throw/signal formatting, and signal payload fidelity.
- GNU Emacs reference tree: `/home/exec/Projects/github.com/emacs-mirror/emacs/`

## Fixed in this patch

### Raw signal payload shape now matches GNU Emacs

GNU Emacs preserves the original cdr passed to `signal`. In `src/eval.c`, the error object is constructed as `(ERROR-SYMBOL . DATA)` instead of normalizing DATA into a proper list. That matters for cases like:

```elisp
(condition-case err
    (signal 'error 1)
  (error err))
```

GNU returns:

```elisp
(error . 1)
```

Before this patch, neomacs normalized non-list signal payloads into a `Vec<Value>` and lost the original cdr shape. That broke:
- `condition-case` bindings for raw/non-list payloads
- display/formatting of signaled errors
- rethrowing/conversion paths that round-tripped `EvalError` back into `Flow`

The fix introduces `raw_data` alongside the normalized vector payload and preserves it through:
- `neovm-core/src/emacs_core/error.rs`
- `neovm-core/src/emacs_core/errors.rs`
- `neovm-core/src/emacs_core/eval.rs`
- `neovm-core/src/emacs_core/load.rs`
- `neovm-core/src/emacs_core/lread.rs`
- `neovm-core/src/emacs_core/builtins/misc_eval.rs`
- `neovm-worker/src/lib.rs`

Focused regression tests were added for:
- raw payload preservation in `builtin_signal`
- `condition-case` binding shape for `(signal 'error 1)`
- formatter output for raw payload signals

## GNU design and implementation

### Lisp macros only define syntax

GNU keeps the Lisp-facing surface of this subsystem in `lisp/subr.el`, but the
runtime semantics are owned by C:

- `handler-bind` is only a macro wrapper around `handler-bind-1`
- `condition-case-unless-debug` is only a macro rewrite around
  `condition-case`

That means the actual behavior is not "implemented in Lisp" even though the
user-level entry points are Lisp macros.

Important GNU ownership:

- `lisp/subr.el`
  - `handler-bind`
  - `condition-case-unless-debug`
- `src/eval.c`
  - `Fhandler_bind_1`
  - `internal_lisp_condition_case`
  - `signal_or_quit`
  - `find_handler_clause`
  - `maybe_call_debugger`
  - `push_handler`
- `src/lisp.h`
  - `enum handlertype`
  - `struct handler`

### One unified handler stack owns signals, catches, and debugger entry

GNU does not treat `catch`, `condition-case`, `handler-bind`, and debugger
entry as separate subsystems.

`src/lisp.h` defines a single `struct handler` chain with:

- `CATCHER`
- `CONDITION_CASE`
- `CATCHER_ALL`
- `HANDLER_BIND`
- `SKIP_CONDITIONS`

This is the key design choice.  Non-local control is not first caught by
special forms and then post-processed.  Instead, `signal_or_quit` walks one
active handler chain and decides what happens next.

### `handler-bind` is part of signal dispatch, not a wrapper retry

GNU `Fhandler_bind_1` pushes `HANDLER_BIND` entries before calling the body
function.  The handler is therefore active at the instant the signal is
searched.

When `signal_or_quit` reaches a matching `HANDLER_BIND`, it:

1. pushes `SKIP_CONDITIONS`
2. calls the handler immediately with the full error object
3. pops the temporary mask
4. continues searching

The `SKIP_CONDITIONS` entry is important.  GNU uses it to mute the relevant
`CONDITION_CASE` / `HANDLER_BIND` frames underneath the running handler without
hiding `catch` frames.  This is how GNU gets all of these behaviors at once:

- handler runs inside the signal's dynamic extent
- lower condition handlers are temporarily muted
- handlers do not recursively apply to code run inside themselves
- non-error nonlocal exits like `throw` are still visible

This behavior is directly documented by GNU's own comments in `src/lisp.h` and
tested in `test/src/eval-tests.el`.

### `condition-case` is also installed into the same chain

GNU `internal_lisp_condition_case` validates handlers, reverses the clause
table, and pushes `CONDITION_CASE` frames using `setjmp`/`longjmp` machinery.
When a signal is selected for one of those frames, unwinding lands in the
chosen clause body and binds the error object there.

Two details matter for compatibility:

- `:success` is handled by the same C implementation, not by a separate macro
  layer
- the bound error value is the original error object, i.e.
  `(ERROR-SYMBOL . DATA)`

### Debugger entry is decided after handler search, not before

GNU `signal_or_quit` first searches active handlers, then decides whether the
debugger should still run.

That debugger decision depends on:

- whether any clause matched
- whether the chosen clause contains `debug`
- `debug-on-error` / `debug-on-quit`
- `inhibit-debugger`
- `debugger-ignored-errors`

This is why `condition-case-unless-debug` works.  The macro in `lisp/subr.el`
rewrites each condition into a handler head like `(debug error ...)`.  The
special behavior comes from `signal_or_quit`, not from the macro itself.

So GNU's `debug` symbol in a condition list is not an ordinary error condition.
It is a control marker interpreted by the signal runtime.

### Interpreter and bytecode share one runtime path

GNU's handler chain is also the bridge between the interpreter and bytecode
execution.  `struct handler` stores bytecode state (`bytecode_top`,
`bytecode_dest`) so the same signal search logic can resume the right target.

That means GNU does not duplicate condition dispatch logic across:

- special forms
- bytecode interpreter
- debugger
- `handler-bind`

The language-visible semantics come from one condition runtime.

## Neomacs current ownership

Neomacs currently splits this subsystem across several independent paths:

- interpreter `condition-case`
  - `neovm-core/src/emacs_core/eval.rs`
- wrapper-style `handler-bind-1`
  - `neovm-core/src/emacs_core/builtins/symbols.rs`
- bytecode `condition-case` resumption
  - `neovm-core/src/emacs_core/bytecode/vm.rs`
- standalone debugger state
  - `neovm-core/src/emacs_core/debug.rs`

This is the architectural mismatch.

The current neomacs design does not have a single active condition-handler
stack shared by interpreter, VM, and debugger decisions.  Instead:

- `sf_condition_case` catches `Flow::Signal` after body evaluation returns
- `handler-bind-1` runs handlers only after the body already failed
- VM condition handling is implemented separately in `resume_nonlocal`
- debugger state exists, but it is not part of the actual signal search path

## Consequences for compatibility

Because GNU owns all of this in one runtime path, the following GNU behaviors
are linked and should not be fixed independently:

- `handler-bind` dynamic extent
- `condition-case` binding shape
- muting lower handlers while a handler runs
- debugger suppression vs `(debug ...)` clauses
- bytecode/interpreter parity for condition dispatch

Neomacs can keep patching local symptoms, but 100% GNU compatibility here
probably requires a unified internal condition-dispatch stack.

## Remaining incompatibilities

### `handler-bind` runs too late in neomacs

GNU Emacs installs handler-bind frames into the active handler stack before calling the body function. The handler is therefore invoked inside the dynamic extent of the signaling code. See:
- GNU `src/eval.c`: `push_handler_bind` / `Fhandler_bind_1`
- GNU `test/src/eval-tests.el`: `eval-tests--handler-bind`

Current neomacs behavior is different. `builtin_handler_bind_1` calls the body first, waits for `Err(Flow::Signal(sig))`, and only then walks handlers in Rust:
- `neovm-core/src/emacs_core/builtins/symbols.rs`

That means inner dynamic context has already unwound before the handler runs. GNU specifically guarantees the opposite for `handler-bind`.

Impact:
- inner `catch`
- inner `condition-case`
- dynamic bindings visible at signal time

These can behave differently under neomacs even when the same handler eventually runs.

This is not a small local fix. To match GNU, handler-bind needs to participate directly in the evaluator's nonlocal-exit search, not as an after-the-fact retry layer.

### `condition-case` does not implement GNU's special `debug` condition semantics

GNU Lisp uses a special `debug` condition to let handlers coexist with debugger entry. `condition-case-unless-debug` in `lisp/subr.el` rewrites handlers to include `debug`, relying on runtime debugger-aware signal handling.

Current neomacs `sf_condition_case` only performs structural/hierarchical matching against the signaled condition:
- `neovm-core/src/emacs_core/eval.rs`

It does not implement the debugger-aware `debug` path, and it does not consult `debug-on-error` / `debugger` while choosing handlers. That means macros depending on GNU's `debug` condition semantics are not yet truly compatible.

## Verification notes

GNU reference check used:

```sh
/home/exec/Projects/github.com/emacs-mirror/emacs/src/emacs --batch -Q -l /tmp/neomacs-oracle-read-file.el /tmp/neomacs-signal-raw.forms
```

Observed GNU result:

```elisp
(error . 1)
```

`cargo nextest` on the broader oracle slice remains too expensive for this area under the current parallel setup and produced many timeouts, so this patch uses focused regression coverage instead of treating those timeouts as semantic failures.

## Recommended next steps

1. Introduce a unified condition-handler stack owned by the evaluator runtime, not by individual special forms or wrapper builtins.
2. Rework `handler-bind` so handler frames live in that active stack during body execution, with GNU-style masking of lower condition handlers.
3. Move debugger entry decisions into the same signal-dispatch path, including GNU's special handling of `debug` and `debugger-ignored-errors`.
4. Reconcile interpreter and VM condition dispatch so both consume the same handler model instead of separate implementations.
5. Split large oracle/property suites for error handling into smaller nextest-friendly groups so compatibility regressions stop hiding behind timeouts.
